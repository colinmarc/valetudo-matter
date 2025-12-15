rs_matter::import!(
    Identify,
    RvcRunMode,
    RvcCleanMode,
    RvcOperationalState,
    // ServiceArea
);

#[allow(dead_code)]
#[allow(clippy::all)]
pub(crate) mod valetudo {
    include!(concat!(env!("OUT_DIR"), "/_valetudo_openapi.rs"));
}
