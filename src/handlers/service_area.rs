use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::{Nullable, TLVBuilderParent},
};

use rs_matter::dm::clusters::decl::service_area::{self, SelectAreasStatus};

use crate::device::{Device, segment_area_id};

impl service_area::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = service_area::FULL_CLUSTER
        .with_revision(2)
        .with_features(1) // SELRUN: allow changing areas while running
        .with_attrs(rs_matter::with!(required));

    fn dataver(&self) -> u32 {
        self.current_state.dataver()
    }

    fn dataver_changed(&self) {
        self.current_state.dataver_changed()
    }

    async fn supported_areas<P: TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: ArrayAttributeRead<
            service_area::AreaStructArrayBuilder<P>,
            service_area::AreaStructBuilder<P>,
        >,
    ) -> Result<P, Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for segment in &self.segments {
                    let Some(area_id) = segment_area_id(segment) else {
                        continue;
                    };
                    builder = build_area(builder.push()?, area_id, segment.name.as_deref())?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let segment = self
                    .segments
                    .iter()
                    .filter(|s| segment_area_id(s).is_some())
                    .nth(index as usize)
                    .ok_or(Error::new(ErrorCode::ConstraintError))?;

                build_area(
                    builder,
                    segment_area_id(segment).unwrap(),
                    segment.name.as_deref(),
                )
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    async fn selected_areas<P: TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::ReadContext,
        builder: ArrayAttributeRead<
            rs_matter::tlv::ToTLVArrayBuilder<P, u32>,
            rs_matter::tlv::ToTLVBuilder<P, u32>,
        >,
    ) -> Result<P, Error> {
        let selected = self.selected_areas.borrow();
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for &area_id in selected.iter() {
                    builder = builder.push(&area_id)?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let area_id = selected
                    .get(index as usize)
                    .ok_or(Error::new(ErrorCode::ConstraintError))?;

                builder.set(area_id)
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    async fn handle_select_areas<P: TLVBuilderParent>(
        &self,
        ctx: impl rs_matter::dm::InvokeContext,
        request: service_area::SelectAreasRequest<'_>,
        response: service_area::SelectAreasResponseBuilder<P>,
    ) -> Result<P, Error> {
        let valid_ids: Vec<u32> = self.segments.iter().filter_map(segment_area_id).collect();

        let mut new_areas = Vec::new();
        for area_id in request.new_areas()? {
            let area_id = area_id?;
            if !valid_ids.contains(&area_id) {
                return response
                    .status(SelectAreasStatus::UnsupportedArea as _)?
                    .status_text("Unknown area")?
                    .end();
            }
            new_areas.push(area_id);
        }

        *self.selected_areas.borrow_mut() = new_areas;
        ctx.notify_own_cluster_changed();

        response
            .status(SelectAreasStatus::Success as _)?
            .status_text("")?
            .end()
    }

    async fn handle_skip_area<P: TLVBuilderParent>(
        &self,
        _ctx: impl rs_matter::dm::InvokeContext,
        _request: service_area::SkipAreaRequest<'_>,
        response: service_area::SkipAreaResponseBuilder<P>,
    ) -> Result<P, Error> {
        // Not supported — we don't track per-area progress.
        response
            .status(service_area::SkipAreaStatus::InvalidInMode as _)?
            .status_text("Not supported")?
            .end()
    }
}

fn build_area<P: TLVBuilderParent>(
    builder: service_area::AreaStructBuilder<P>,
    area_id: u32,
    name: Option<&str>,
) -> Result<P, Error> {
    builder
        .area_id(area_id)?
        .map_id(Nullable::none())?
        .area_info()?
        .location_info()?
        .non_null()?
        .location_name(name.unwrap_or(""))?
        .floor_number(Nullable::none())?
        .area_type(Nullable::none())?
        .end()?
        .landmark_info()?
        .null()?
        .end()?
        .end()
}
