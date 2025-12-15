use log::{info, warn};
use rs_matter::{
    dm::Context,
    error::{Error, ErrorCode},
};

use crate::{device::Capability, device::Device, generated::identify, handlers::to_matter_err};

// pub(crate) struct IdentifyHandler<'a> {
//     device: &'a Device,
//     dataver: Dataver,
//     identify_time: Cell<u16>,
// }

impl Device {
    async fn identify_robot(&self, ctx: impl Context, dur: u16) -> Result<(), Error> {
        let old_value = self.identify_time.replace_notify(dur, &ctx);

        if dur == 0 || old_value != 0 {
            return Ok(());
        }

        let res = if self
            .capabilities
            .contains(Capability::SpeakerTestCapability)
        {
            info!("playing test sound in response to identification request");
            self.client
                .put(
                    "/api/v2/robot/capabilities/SpeakerTestCapability",
                    r#"{"action": "play_test_sound"}"#.to_owned(),
                )
                .await
                .map_err(to_matter_err)
        } else {
            warn!("no SpeakerTestCapability, not playing identification sound");
            Ok(())
        };

        self.identify_time.set_notify(0, ctx);
        res
    }
}

impl identify::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = identify::FULL_CLUSTER
        .with_revision(6)
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.identify_time.dataver()
    }

    fn dataver_changed(&self) {
        self.identify_time.dataver_changed();
    }

    async fn identify_time(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<u16, rs_matter::error::Error> {
        Ok(self.identify_time.get())
    }

    async fn identify_type(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<identify::IdentifyTypeEnum, rs_matter::error::Error> {
        Ok(identify::IdentifyTypeEnum::AudibleBeep)
    }

    async fn set_identify_time(
        &self,
        ctx: impl rs_matter::dm::WriteContext,
        value: u16,
    ) -> Result<(), rs_matter::error::Error> {
        self.identify_robot(ctx, value).await
    }

    async fn handle_identify(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        request: identify::IdentifyRequest<'_>,
    ) -> Result<(), rs_matter::error::Error> {
        self.identify_robot(ctx, request.identify_time()?).await
    }

    async fn handle_trigger_effect(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        _request: identify::TriggerEffectRequest<'_>,
    ) -> Result<(), rs_matter::error::Error> {
        Err(ErrorCode::InvalidCommand.into()) // Unimplemented.
    }
}
