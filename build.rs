fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=proto/pay/v1/pay.proto");
    prost_build::Config::new()
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(".", "#[serde(default, deny_unknown_fields)]")
        .compile_protos(&["proto/pay/v1/pay.proto"], &["proto"])
}
