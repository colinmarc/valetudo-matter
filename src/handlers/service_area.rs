use rs_matter::{
    dm::ArrayAttributeRead,
    error::{Error, ErrorCode},
    tlv::{Nullable, TLVBuilderParent},
};

use rs_matter::dm::clusters::decl::service_area::{self, SelectAreasStatus};

use crate::device::{Device, MapSegment, StatusValue, segment_area_id};

impl service_area::ClusterAsyncHandler for Device {
    const CLUSTER: rs_matter::dm::Cluster<'static> = service_area::FULL_CLUSTER
        .with_revision(2)
        .with_features(0) // No API exists to change an active job or skip its current area.
        .with_cmds(rs_matter::with!(service_area::CommandId::SelectAreas))
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
        let segments = self.segments.borrow();
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for segment in segments.iter() {
                    let Some(area_id) = segment_area_id(segment) else {
                        continue;
                    };
                    builder = build_area(builder.push()?, area_id, segment.name.as_deref())?;
                }

                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let segment = segments
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
        let valid_ids: Vec<u32> = self
            .segments
            .borrow()
            .iter()
            .filter_map(segment_area_id)
            .collect();

        let requested = request
            .new_areas()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let can_change = !self.cycle_active()
            && matches!(
                self.current_state.get().value,
                StatusValue::Idle | StatusValue::Docked
            );
        let new_areas = match validate_selection(
            &self.selected_areas.borrow(),
            requested,
            &valid_ids,
            can_change,
        ) {
            Ok(areas) => areas,
            Err(status) => {
                return response
                    .status(status as _)?
                    .status_text(if matches!(status, SelectAreasStatus::UnsupportedArea) {
                        "Unknown area"
                    } else {
                        "Selection can only change while idle"
                    })?
                    .end();
            }
        };
        if *self.selected_areas.borrow() == new_areas {
            // Mandatory successful no-op, including while another fabric's
            // command is awaiting HTTP. Explicitly reselecting also acknowledges
            // any earlier map invalidation without altering the pending scope.
            self.selection_invalidated.set(false);
            return response
                .status(SelectAreasStatus::Success as _)?
                .status_text("")?
                .end();
        }
        let Some(_command) = self.try_command() else {
            return response
                .status(SelectAreasStatus::InvalidInMode as _)?
                .status_text("Another command is in progress")?
                .end();
        };
        *self.selected_areas.borrow_mut() = new_areas;
        self.selection_invalidated.set(false);
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
    let name = area_name(name, area_id);
    builder
        .area_id(area_id)?
        .map_id(Nullable::none())?
        .area_info()?
        .location_info()?
        .non_null()?
        .location_name(&name)?
        .floor_number(Nullable::none())?
        .area_type(Nullable::none())?
        .end()?
        .landmark_info()?
        .null()?
        .end()?
        .end()
}

fn validate_selection(
    current: &[u32],
    requested: Vec<u32>,
    valid: &[u32],
    can_change: bool,
) -> Result<Vec<u32>, SelectAreasStatus> {
    let mut unique = Vec::new();
    for id in requested {
        if !valid.contains(&id) {
            return Err(SelectAreasStatus::UnsupportedArea);
        }
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    // Matter requires an identical selection to succeed even during a job.
    if unique != current && !can_change {
        return Err(SelectAreasStatus::InvalidInMode);
    }
    Ok(unique)
}

fn unambiguous_segments(segments: Vec<MapSegment>) -> Vec<MapSegment> {
    let mut valid: Vec<_> = segments
        .iter()
        .filter(|segment| {
            segment_area_id(segment).is_some_and(|id| {
                segments
                    .iter()
                    .filter(|other| segment_area_id(other) == Some(id))
                    .count()
                    == 1
            })
        })
        .cloned()
        .collect();
    valid.sort_by_key(|segment| segment_area_id(segment).unwrap());
    valid.truncate(255);
    let names: Vec<_> = valid
        .iter()
        .map(|segment| area_name(segment.name.as_deref(), segment_area_id(segment).unwrap()))
        .collect();
    // Without maps, AreaInfo must also be unique, not only AreaID. A stable ID
    // suffix disambiguates equal or truncation-colliding user names.
    let disambiguate = names
        .iter()
        .enumerate()
        .any(|(index, name)| names[..index].contains(name));
    for (segment, mut name) in valid.iter_mut().zip(names) {
        if disambiguate {
            let suffix = format!(" [{}]", segment_area_id(segment).unwrap());
            let mut end = name.len().min(128 - suffix.len());
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
            name.push_str(&suffix);
        }
        segment.name = Some(name);
    }
    valid
}

/// LocationName is a UTF-8 string of at most 128 bytes. A nonempty label is
/// required when floor, area type and landmark are all absent.
fn area_name(name: Option<&str>, id: u32) -> String {
    let name = name.filter(|name| !name.trim().is_empty());
    match name {
        None => format!("Area {id}"),
        Some(name) => {
            let mut end = name.len().min(128);
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name[..end].to_owned()
        }
    }
}

impl Device {
    pub(crate) fn normalize_segments(segments: Vec<MapSegment>) -> Vec<MapSegment> {
        unambiguous_segments(segments)
    }

    pub(crate) async fn refresh_segments(&self, notify: &impl rs_matter::dm::AttrChangeNotifier) {
        match self
            .client
            .get_optional::<Vec<MapSegment>>("/api/v2/robot/capabilities/MapSegmentationCapability")
            .await
        {
            Ok(segments) => {
                let valid = unambiguous_segments(segments);
                if *self.segments.borrow() != valid {
                    let old_count = self.selected_areas.borrow().len();
                    self.selected_areas
                        .borrow_mut()
                        .retain(|id| valid.iter().any(|s| segment_area_id(s) == Some(*id)));
                    if self.selected_areas.borrow().len() != old_count {
                        // An empty selection means full clean. Losing a selected
                        // room must never silently expand the command's scope.
                        self.selection_invalidated.set(true);
                    }
                    *self.segments.borrow_mut() = valid;
                    self.current_state.dataver_changed();
                    notify.notify_cluster_changed(1, service_area::FULL_CLUSTER.id);
                }
            }
            Err(error) => log::warn!("Cannot refresh map segments: {error:#}"),
        }
    }

    /// Refresh only from the public capability API. Transient errors retain the
    /// last valid list; a successful empty list means the map was removed.
    pub(crate) async fn monitor_segments(&self, notify: &impl rs_matter::dm::AttrChangeNotifier) {
        if !self
            .capabilities
            .contains(crate::device::Capability::MapSegmentationCapability)
        {
            std::future::pending::<()>().await;
        }
        loop {
            self.refresh_segments(notify).await;
            smol::Timer::after(std::time::Duration::from_secs(15)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_deduplicates_in_order_and_rejects_active_changes() {
        assert_eq!(
            validate_selection(&[], vec![3, 2, 2, 1, 2], &[1, 2, 3], true).unwrap(),
            vec![3, 2, 1]
        );
        assert!(matches!(
            validate_selection(&[1], vec![2], &[1, 2], false),
            Err(SelectAreasStatus::InvalidInMode)
        ));
        assert_eq!(
            validate_selection(&[1], vec![1, 1], &[1, 2], false).unwrap(),
            vec![1]
        );
        assert!(matches!(
            validate_selection(&[], vec![99], &[1, 2], true),
            Err(SelectAreasStatus::UnsupportedArea)
        ));
        assert_eq!(
            validate_selection(&[1], vec![], &[1, 2], true).unwrap(),
            Vec::<u32>::new()
        );
    }
    #[test]
    fn ambiguous_ids_never_target_arbitrary_rooms() {
        let segments = ["1", "01", "2", "room"]
            .into_iter()
            .map(|id| MapSegment {
                id: id.into(),
                name: None,
            })
            .collect();
        let valid = unambiguous_segments(segments);
        assert_eq!(valid.len(), 2);
        assert_eq!(valid[0].id, "2");
        assert_eq!(valid[1].id, "room");
        let hash = segment_area_id(&valid[1]).unwrap();
        assert!(
            unambiguous_segments(vec![
                valid[1].clone(),
                MapSegment {
                    id: hash.to_string(),
                    name: None
                }
            ])
            .is_empty()
        );
    }
    #[test]
    fn normalized_area_info_is_unique_and_list_bounded() {
        let segments = (0..300)
            .map(|id| MapSegment {
                id: id.to_string(),
                name: Some("Kitchen".into()),
            })
            .collect();
        let valid = unambiguous_segments(segments);
        assert_eq!(valid.len(), 255);
        let names: std::collections::HashSet<_> =
            valid.iter().map(|s| s.name.as_deref().unwrap()).collect();
        assert_eq!(names.len(), 255);
        assert!(names.contains("Kitchen [0]"));
    }
    #[test]
    fn names_are_nonempty_and_utf8_bounded() {
        assert_eq!(area_name(None, 16), "Area 16");
        assert_eq!(area_name(Some("  "), 17), "Area 17");
        let value = area_name(Some(&"ă".repeat(80)), 1);
        assert_eq!(value.len(), 128);
        assert_eq!(value.chars().count(), 64);
    }
    #[test]
    fn skip_and_select_while_running_are_not_advertised() {
        let cluster = <Device as service_area::ClusterAsyncHandler>::CLUSTER;
        assert_eq!(cluster.feature_map, 0);
        assert_eq!(
            cluster
                .commands
                .iter()
                .filter(|cmd| (cluster.with_cmds)(cmd, cluster.revision, cluster.feature_map))
                .count(),
            1
        );
    }
}
