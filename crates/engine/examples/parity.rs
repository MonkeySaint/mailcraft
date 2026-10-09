//! Prints the catalog parity report: `cargo run -p mailcraft-engine --example parity [-- --full]`.

use serde_json::Value;

fn field<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

fn main() {
    let reg = mailcraft_engine::cmd::registry();
    let p = mailcraft_engine::catalog::parity(&reg);
    if std::env::args().any(|a| a == "--full") {
        println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());
        return;
    }
    println!("registry: {} commands", reg.len());
    for key in ["total_rows", "not_applicable", "mapped_rows", "implemented_rows", "percent"] {
        println!("{key}: {}", field(&p, key));
    }
    if let Some(areas) = field(&p, "by_area").as_object() {
        for (area, t) in areas {
            println!("  {area}: {}/{}", field(t, "implemented"), field(t, "mapped"));
        }
    }
    let missing: Vec<&str> = field(&p, "missing_ids")
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    println!("missing ids ({}): {}", missing.len(), missing.join(", "));
}
