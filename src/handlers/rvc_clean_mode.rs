use crate::{device::Device, generated::rvc_clean_mode};

impl rvc_clean_mode::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = rvc_clean_mode::FULL_CLUSTER
        .with_revision(5)
        .with_features(20) // DIRECTMODECH
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        todo!()
    }

    fn dataver_changed(&self) {
        todo!()
    }

    async fn supported_modes<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        _builder: rs_matter::dm::ArrayAttributeRead<
            rvc_clean_mode::ModeOptionStructArrayBuilder<P>,
            rvc_clean_mode::ModeOptionStructBuilder<P>,
        >,
    ) -> Result<P, rs_matter::error::Error> {
        todo!()
    }

    async fn current_mode(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<u8, rs_matter::error::Error> {
        todo!()
    }

    async fn handle_change_to_mode<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        _request: rvc_clean_mode::ChangeToModeRequest<'_>,
        _response: rvc_clean_mode::ChangeToModeResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        todo!()
    }
}
