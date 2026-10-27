//! Opt-in, bounded audit of actual RVC reads. No session keys or other clusters.
use rs_matter::{
    dm::{
        AsyncHandler, HandlerContext, InvokeContext, InvokeReply, ReadContext, ReadReply, Reply,
        WriteContext,
    },
    error::Error,
    tlv::{TLVWrite, TagType, ToTLV},
};

pub(crate) struct Audit<H>(pub H);
impl<H: AsyncHandler> AsyncHandler for Audit<H> {
    fn read_awaits(&self, ctx: impl ReadContext) -> bool {
        self.0.read_awaits(ctx)
    }
    async fn read(&self, ctx: impl ReadContext, reply: impl ReadReply) -> Result<(), Error> {
        if std::env::var("VALETUDO_MATTER_AUDIT_RVC").as_deref() != Ok("1") {
            return self.0.read(ctx, reply).await;
        }
        let a = ctx.attr();
        let meta = format!(
            "fabric={} ep={} cluster=0x{:04x} attr=0x{:04x} index={:?} filter={:?}",
            a.fab_idx, a.endpoint_id, a.cluster_id, a.attr_id, a.list_index, a.dataver
        );
        log::info!(target:"matter_audit", "READ {meta}");
        let result = self
            .0
            .read(
                ctx,
                AuditRead {
                    inner: reply,
                    meta: meta.clone(),
                },
            )
            .await;
        if let Err(e) = &result {
            log::info!(target:"matter_audit", "ERROR {meta} {e:?}");
        }
        result
    }
    fn write_awaits(&self, ctx: impl WriteContext) -> bool {
        self.0.write_awaits(ctx)
    }
    async fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
        self.0.write(ctx).await
    }
    fn invoke_awaits(&self, ctx: impl InvokeContext) -> bool {
        self.0.invoke_awaits(ctx)
    }
    async fn invoke(&self, ctx: impl InvokeContext, reply: impl InvokeReply) -> Result<(), Error> {
        self.0.invoke(ctx, reply).await
    }
    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        self.0.run(ctx).await
    }
}
struct AuditRead<R> {
    inner: R,
    meta: String,
}
impl<R: ReadReply> ReadReply for AuditRead<R> {
    fn with_dataver(self, dataver: u32) -> Result<Option<impl Reply>, Error> {
        let inner = self.inner.with_dataver(dataver)?;
        log::info!(target:"matter_audit", "VERSION {} version={} filtered={}",self.meta,dataver,inner.is_none());
        Ok(inner.map(|inner| AuditReply {
            inner,
            meta: self.meta,
            data: Vec::new(),
        }))
    }
}
struct AuditReply<R> {
    inner: R,
    meta: String,
    data: Vec<u8>,
}
impl<R: Reply> Reply for AuditReply<R> {
    const TAG: TagType = R::TAG;
    fn set<T: ToTLV>(mut self, value: T) -> Result<(), Error> {
        value.to_tlv(&Self::TAG, self.writer())?;
        self.complete()
    }
    fn reset(&mut self) {
        self.inner.reset();
        self.data.clear();
    }
    fn writer(&mut self) -> impl TLVWrite + Send + '_ {
        Tee {
            inner: self.inner.writer(),
            data: &mut self.data,
        }
    }
    fn complete(self) -> Result<(), Error> {
        self.inner.complete()?;
        let hex: String = self.data.iter().map(|b| format!("{b:02x}")).collect();
        log::info!(target:"matter_audit", "ENCODED {} value_tlv={hex}",self.meta);
        Ok(())
    }
}
struct Tee<'a, W> {
    inner: W,
    data: &'a mut Vec<u8>,
}
impl<W: TLVWrite> TLVWrite for Tee<'_, W> {
    type Position = (W::Position, usize);
    fn write(&mut self, byte: u8) -> Result<(), Error> {
        self.inner.write(byte)?;
        if self.data.len() < 4096 {
            self.data.push(byte);
        }
        Ok(())
    }
    fn get_tail(&self) -> Self::Position {
        (self.inner.get_tail(), self.data.len())
    }
    fn rewind_to(&mut self, pos: Self::Position) {
        self.inner.rewind_to(pos.0);
        self.data.truncate(pos.1);
    }
    fn available_space(&mut self) -> &mut [u8] {
        self.inner.available_space()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rs_matter::{tlv::TLVTag, utils::storage::WriteBuf};
    #[test]
    fn tee_preserves_real_bytes_and_rewinds_capture() {
        let mut buf = [0u8; 128];
        let mut writer = WriteBuf::new(&mut buf);
        let mut captured = Vec::new();
        {
            let mut tee = Tee {
                inner: &mut writer,
                data: &mut captured,
            };
            tee.start_array(&TLVTag::Anonymous).unwrap();
            let anchor = tee.get_tail();
            tee.u8(&TLVTag::Anonymous, 99).unwrap();
            tee.rewind_to(anchor);
            tee.u8(&TLVTag::Anonymous, 4).unwrap();
            tee.end_container().unwrap();
        }
        assert_eq!(captured, writer.as_slice());
        assert_eq!(captured, vec![0x16, 0x04, 4, 0x18]);
    }
}
