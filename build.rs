use std::{env, fs, path::Path};

use anyhow::anyhow;
use typify::{TypeSpace, TypeSpaceSettings};

fn main() -> anyhow::Result<()> {
    let content =
        std::fs::read_to_string("openapi-schema.v2025.12.0.json").expect("missing schema file");
    let spec = serde_json::from_str::<serde_json::Value>(&content).unwrap();

    let mut type_space = TypeSpace::new(TypeSpaceSettings::default().with_struct_builder(true));
    let schemas = spec["components"]["schemas"]
        .as_object()
        .ok_or(anyhow!("'schemas' is not a map"))?;

    for (k, v) in schemas {
        let schema: schemars::schema::Schema = serde_json::from_value(v.clone())?;
        type_space.add_type_with_name(&schema, Some(k.clone()))?;
    }

    let paths = spec["paths"]
        .as_object()
        .ok_or(anyhow!("'paths' is not a map"))?;
    for (path, path_spec) in paths {
        let response_schema =
            &path_spec["get"]["responses"]["200"]["content"]["application/json"]["schema"];
        if !response_schema.is_object() {
            eprintln!("No response schema for endpoint {path}");
            continue;
        };

        let response_schema: schemars::schema::Schema =
            serde_json::from_value(response_schema.clone())?;

        let name = path_to_name(path);
        type_space.add_type_with_name(&response_schema, Some(name))?;
    }

    let contents =
        prettyplease::unparse(&syn::parse2::<syn::File>(type_space.to_stream()).unwrap());

    let mut out_file = Path::new(&env::var("OUT_DIR").unwrap()).to_path_buf();
    out_file.push("_valetudo_openapi.rs");

    fs::write(out_file, contents)?;
    Ok(())
}

fn path_to_name(path: &str) -> String {
    let path = path.trim_start_matches('/');
    let parts: Vec<&str> = path.split('/').collect();

    // Expected format: api/v{N}/segment1/segment2/...
    if parts.len() < 3 || parts[0] != "api" || !parts[1].starts_with('v') {
        return String::new(); // or handle error as needed
    }

    let version = &parts[1][1..]; // Extract number from "v2"
    let segments = &parts[2..];

    let name: String = segments.iter().map(|s| capitalize(s)).collect();
    format!("{name}ResponseV{version}")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => String::new(),
    }
}
