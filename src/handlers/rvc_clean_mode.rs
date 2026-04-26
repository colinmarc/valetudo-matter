use num_enum::{IntoPrimitive, TryFromPrimitive};
use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::TLVBuilderParent,
};

use rs_matter::dm::clusters::decl::{self as decl, rvc_clean_mode};

use crate::{
    device::{Device, Preset},
    handlers::to_matter_err,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
enum CleanMode {
    Vacuum = 0,
    Mop = 1,
    VacuumAndMop = 2,
    VacuumThenMop = 3,
}

impl From<Preset> for CleanMode {
    fn from(preset: Preset) -> Self {
        match preset {
            Preset::Vacuum => CleanMode::Vacuum,
            Preset::Mop => CleanMode::Mop,
            Preset::VacuumAndMop => CleanMode::VacuumAndMop,
            Preset::VacuumThenMop => CleanMode::VacuumThenMop,
        }
    }
}

impl From<CleanMode> for Preset {
    fn from(mode: CleanMode) -> Self {
        match mode {
            CleanMode::Vacuum => Preset::Vacuum,
            CleanMode::Mop => Preset::Mop,
            CleanMode::VacuumAndMop => Preset::VacuumAndMop,
            CleanMode::VacuumThenMop => Preset::VacuumThenMop,
        }
    }
}

impl rvc_clean_mode::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = rvc_clean_mode::FULL_CLUSTER
        .with_revision(5)
        .with_features(20) // DIRECTMODECH
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.current_preset.dataver()
    }

    fn dataver_changed(&self) {
        self.current_preset.dataver_changed();
    }

    async fn supported_modes<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: ArrayAttributeRead<
            decl::globals::ModeOptionStructArrayBuilder<P>,
            decl::globals::ModeOptionStructBuilder<P>,
        >,
    ) -> Result<P, rs_matter::error::Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for preset in self.cleaning_presets.iter() {
                    builder = build_mode(builder.push()?, preset.into())?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let preset = self
                    .cleaning_presets
                    .iter()
                    .nth(index as usize)
                    .ok_or(Error::new(ErrorCode::ConstraintError))?;

                build_mode(builder, preset.into())
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    async fn current_mode(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
    ) -> Result<u8, rs_matter::error::Error> {
        let mode: CleanMode = self.current_preset.get().into();
        Ok(mode.into())
    }

    async fn handle_change_to_mode<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        request: rvc_clean_mode::ChangeToModeRequest<'_>,
        response: rvc_clean_mode::ChangeToModeResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let new_mode: CleanMode = request
            .new_mode()?
            .try_into()
            .map_err(|_| Error::new(ErrorCode::ConstraintError))?;

        let preset: Preset = new_mode.into();
        if !self.cleaning_presets.contains(preset) {
            return Err(Error::new(ErrorCode::ConstraintError));
        }

        self.set_preset(preset).await.map_err(to_matter_err)?;
        self.current_preset.set(preset);

        response.status(0)?.status_text(None)?.end()
    }
}

fn build_mode<P: TLVBuilderParent>(
    builder: decl::globals::ModeOptionStructBuilder<P>,
    mode: CleanMode,
) -> Result<P, Error> {
    let label = match mode {
        CleanMode::Vacuum => "Vacuum",
        CleanMode::Mop => "Mop",
        CleanMode::VacuumAndMop => "Vacuum and Mop",
        CleanMode::VacuumThenMop => "Vacuum then Mop",
    };

    let builder = builder.label(label)?.mode(mode.into())?.mode_tags()?;

    // Each mode gets the appropriate tags from the RVC Clean Mode
    // namespace (section 7.3.7.2).
    match mode {
        CleanMode::Vacuum => builder
            .push()?
            .mfg_code(None)?
            .value(rvc_clean_mode::ModeTag::Vacuum as _)?
            .end()?
            .end(),
        CleanMode::Mop => builder
            .push()?
            .mfg_code(None)?
            .value(rvc_clean_mode::ModeTag::Mop as _)?
            .end()?
            .end(),
        CleanMode::VacuumAndMop => builder
            .push()?
            .mfg_code(None)?
            .value(rvc_clean_mode::ModeTag::Vacuum as _)?
            .end()?
            .push()?
            .mfg_code(None)?
            .value(rvc_clean_mode::ModeTag::Mop as _)?
            .end()?
            .end(),
        CleanMode::VacuumThenMop => builder
            .push()?
            .mfg_code(None)?
            .value(rvc_clean_mode::ModeTag::VacuumThenMop as _)?
            .end()?
            .end(),
    }?
    .end()
}
