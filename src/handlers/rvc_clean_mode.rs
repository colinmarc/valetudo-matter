use num_enum::{IntoPrimitive, TryFromPrimitive};
use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::TLVBuilderParent,
};

use rs_matter::dm::clusters::decl::{self as decl, rvc_clean_mode};

use crate::device::{Device, Preset};

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
enum CleanMode {
    Vacuum = 0,
    Mop = 1,
    VacuumAndMop = 2,
    VacuumThenMop = 3,
    Low = 4,
    Medium = 5,
    High = 6,
    Max = 7,
    Off = 8,
}

impl From<Preset> for CleanMode {
    fn from(preset: Preset) -> Self {
        match preset {
            Preset::Low => CleanMode::Low,
            Preset::Medium => CleanMode::Medium,
            Preset::High => CleanMode::High,
            Preset::Max => CleanMode::Max,
            Preset::Off => CleanMode::Off,
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
            CleanMode::Low => Preset::Low,
            CleanMode::Medium => Preset::Medium,
            CleanMode::High => Preset::High,
            CleanMode::Max => Preset::Max,
            CleanMode::Off => Preset::Off,
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
        .with_features(1 << 20) // DIRECTMODECH
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
        let mode: CleanMode = self
            .current_preset
            .get()
            .ok_or(ErrorCode::InvalidAction)?
            .into();
        Ok(mode.into())
    }

    async fn handle_change_to_mode<P: rs_matter::tlv::TLVBuilderParent>(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        request: rvc_clean_mode::ChangeToModeRequest<'_>,
        response: rvc_clean_mode::ChangeToModeResponseBuilder<P>,
    ) -> Result<P, rs_matter::error::Error> {
        let Some(_command) = self.try_command() else {
            return response
                .status(2)?
                .status_text(Some("Another control command is in progress"))?
                .end();
        };

        let Some(new_mode) = CleanMode::try_from(request.new_mode()?).ok() else {
            return response.status(1)?.status_text(Some(""))?.end();
        };
        let preset: Preset = new_mode.into();
        if !self.cleaning_presets.contains(preset) {
            return response.status(1)?.status_text(Some(""))?.end();
        }
        if self.current_preset.get() != Some(preset) {
            if let Err(error) = self.set_preset(preset).await {
                log::warn!("Clean mode command failed: {error:#}");
                return response
                    .status(2)?
                    .status_text(Some("Valetudo rejected or did not confirm command"))?
                    .end();
            }
            self.current_preset.set_notify(Some(preset), &ctx);
        }

        response.status(0)?.status_text(None)?.end()
    }
}

fn build_mode<P: TLVBuilderParent>(
    builder: decl::globals::ModeOptionStructBuilder<P>,
    mode: CleanMode,
) -> Result<P, Error> {
    let label = match mode {
        CleanMode::Low => "Quiet",
        CleanMode::Medium => "Balanced",
        CleanMode::High => "Turbo",
        CleanMode::Max => "Max",
        CleanMode::Off => "Fan off",
        CleanMode::Vacuum => "Vacuum",
        CleanMode::Mop => "Mop",
        CleanMode::VacuumAndMop => "Vacuum and Mop",
        CleanMode::VacuumThenMop => "Vacuum then Mop",
    };

    let mut tags = builder.label(label)?.mode(mode.into())?.mode_tags()?;
    for value in mode_tags(mode) {
        tags = tags.push()?.mfg_code(None)?.value(*value)?.end()?;
    }
    tags.end()?.end()
}

fn mode_tags(mode: CleanMode) -> &'static [u16] {
    // Matter 1.6.1 Application 1.10.8 and 7.3.7–7.3.9.
    // Home derives intensity choices from these semantic tags, not Labels.
    // Fixed Balanced is not Auto, and stronger suction is not faster cleaning.
    match mode {
        CleanMode::Low => &[0x4001, 0x0003], // Vacuum + LowNoise
        CleanMode::Medium | CleanMode::Vacuum => &[0x4001],
        CleanMode::High => &[0x4001, 0x4000], // normative Turbo/Vacuum example
        CleanMode::Max => &[0x4001, 0x0007],
        CleanMode::Off => &[0x0006], // no Vacuum tag when suction is disabled
        CleanMode::Mop => &[0x4002],
        CleanMode::VacuumAndMop => &[0x4001, 0x4002],
        CleanMode::VacuumThenMop => &[0x4003],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialized_supported_modes_contains_five_real_entries() {
        use rs_matter::{
            tlv::{TLVElement, TLVTag, TLVWriteParent},
            utils::storage::WriteBuf,
        };
        let mut buffer = [0u8; 1024];
        let mut writer = WriteBuf::new(&mut buffer);
        let parent = TLVWriteParent::new((), &mut writer);
        let mut array =
            decl::globals::ModeOptionStructArrayBuilder::new(parent, &TLVTag::Context(2)).unwrap();
        for mode in [
            CleanMode::Low,
            CleanMode::Medium,
            CleanMode::High,
            CleanMode::Max,
            CleanMode::Off,
        ] {
            array = build_mode(array.push().unwrap(), mode).unwrap();
        }
        array.end().unwrap();
        let entries = TLVElement::new(writer.as_slice())
            .array()
            .unwrap()
            .iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 5);
        let expected = [
            (4, "Quiet", vec![0x4001, 3]),
            (5, "Balanced", vec![0x4001]),
            (6, "Turbo", vec![0x4001, 0x4000]),
            (7, "Max", vec![0x4001, 7]),
            (8, "Fan off", vec![6]),
        ];
        for (entry, (id, label, tags)) in entries.iter().zip(expected) {
            let fields = entry.structure().unwrap();
            assert_eq!(fields.find_ctx(0).unwrap().utf8().unwrap(), label);
            assert_eq!(fields.find_ctx(1).unwrap().u8().unwrap(), id);
            let actual = fields
                .find_ctx(2)
                .unwrap()
                .array()
                .unwrap()
                .iter()
                .map(|tag| {
                    tag.unwrap()
                        .structure()
                        .unwrap()
                        .find_ctx(1)
                        .unwrap()
                        .u16()
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, tags);
        }
        println!(
            "SupportedModes serialized TLV: {}",
            writer
                .as_slice()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }
    #[test]
    fn fan_intensity_tags_are_distinct_and_preserve_preset_ids() {
        let modes = [
            CleanMode::Low,
            CleanMode::Medium,
            CleanMode::High,
            CleanMode::Max,
            CleanMode::Off,
        ];
        for (index, mode) in modes.iter().enumerate() {
            assert_eq!(u8::from(*mode), index as u8 + 4);
            for other in modes.iter().skip(index + 1) {
                assert_ne!(mode_tags(*mode), mode_tags(*other));
            }
        }
        assert!(mode_tags(CleanMode::Low).contains(&0x0003));
        assert!(mode_tags(CleanMode::High).contains(&0x4000));
        assert!(mode_tags(CleanMode::Max).contains(&0x0007));
        assert!(!mode_tags(CleanMode::Off).contains(&0x4001));
    }
    #[test]
    fn fixed_suction_does_not_claim_automatic_or_faster_navigation() {
        for mode in [
            CleanMode::Low,
            CleanMode::Medium,
            CleanMode::High,
            CleanMode::Max,
            CleanMode::Off,
        ] {
            assert!(!mode_tags(mode).contains(&0x0000));
            assert!(!mode_tags(mode).contains(&0x0001));
        }
        assert_eq!(mode_tags(CleanMode::VacuumAndMop), &[0x4001, 0x4002]);
        assert_eq!(mode_tags(CleanMode::VacuumThenMop), &[0x4003]);
    }
}
