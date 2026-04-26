//! Here is a summary of the relevant bits of the matter spec as far as it
//! applies to valetudo.
//!
//!  - Start a cleaning/mapping cycle: change_to_mode called on the "run mode"
//!    cluster, setting the mode to clean or map.
//!  - Stop the cleaning/mapping cycle: change_to_mode called on the "run mode"
//!    cluster, setting the mode to idle. `go_home` on the "operational state"
//!    cluster does the same thing.
//!  - Pause/resume are performed on the "operational state" cluster.
//!  - Any change to the robot state (external or triggered via matter) causes
//!    the `operational_state` to change.
//!  - If the robot becomes stopped/paused/docked/charging/error, the `current_run_mode`
//!    should change to idle.
//!  - If the robot starts cleaning or mapping, the `current_run_mode` should
//!    change to cleaning or mapping, respectively.
//!  - If the robot hits an error, the `current_run_mode` should be idle and the
//!    `operational_state` should be "error". We should also return anything
//!    useful from `operational_error`.

mod identify;
mod rvc_clean_mode;
mod rvc_operational_state;
mod rvc_run_mode;
mod service_area;

use std::cell::Cell;

use rand::RngCore;
use rs_matter::{
    dm::{Dataver, OperationContext},
    error::{Error, ErrorCode::Failure},
};

pub(crate) struct VersionedCell<T: Copy + PartialEq> {
    inner: Cell<T>,
    dataver: Dataver,
}

impl<T: Copy + PartialEq> VersionedCell<T> {
    pub(crate) fn new(inner: T, rand: &mut impl RngCore) -> Self {
        Self {
            inner: Cell::new(inner),
            dataver: Dataver::new_rand(rand),
        }
    }

    pub(crate) fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    pub(crate) fn dataver_changed(&self) {
        self.dataver.changed();
    }

    pub(crate) fn get(&self) -> T {
        self.inner.get()
    }

    pub(crate) fn set(&self, value: T) {
        let old_value = self.inner.replace(value);
        if value != old_value {
            self.dataver.changed();
        }
    }

    pub(crate) fn set_notify(&self, value: T, ctx: &impl OperationContext) {
        let old_value = self.inner.replace(value);
        if value != old_value {
            self.dataver.changed();
            ctx.notify_own_cluster_changed();
        }
    }

    pub(crate) fn replace(&self, value: T) -> T {
        let old_value = self.inner.replace(value);
        if value != old_value {
            self.dataver.changed();
        }

        old_value
    }

    pub(crate) fn replace_notify(&self, value: T, ctx: &impl OperationContext) -> T {
        let old_value = self.replace(value);
        if old_value != value {
            self.dataver.changed();
            ctx.notify_own_cluster_changed();
        }

        old_value
    }
}

fn to_matter_err(err: anyhow::Error) -> Error {
    Error::new_with_details(Failure, err.into_boxed_dyn_error())
}
