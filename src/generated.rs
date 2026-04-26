pub(crate) use rs_matter::dm::clusters::decl::{
    identify, rvc_clean_mode, rvc_operational_state, rvc_run_mode,
};

#[allow(dead_code)]
#[allow(clippy::all)]
pub(crate) mod valetudo {
    include!(concat!(env!("OUT_DIR"), "/_valetudo_openapi.rs"));
}
