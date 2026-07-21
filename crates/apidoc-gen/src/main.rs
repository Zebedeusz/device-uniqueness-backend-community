//! `apidoc-gen` — regenerate the committed OpenAPI document and API reference.
//!
//! Writes `docs/api-reference/openapi.json` and `docs/api-reference/index.html`
//! from the service crates' `#[utoipa::path]` annotations. Run via `just openapi`.

use std::fs;

fn main() -> anyhow::Result<()> {
    let json = apidoc_gen::openapi_json();
    fs::write(apidoc_gen::openapi_json_path(), &json)?;
    println!("wrote {}", apidoc_gen::openapi_json_path());

    let html = apidoc_gen::index_html()?;
    fs::write(apidoc_gen::index_html_path(), &html)?;
    println!("wrote {}", apidoc_gen::index_html_path());

    Ok(())
}
