use serde_json::Value;
use std::{
    env,
    error::Error,
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

fn main() {
    if let Err(error) = package() {
        eprintln!("xtask: {error}");
        std::process::exit(1);
    }
}

fn package() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("package") {
        return Err("usage: cargo xtask package [cargo build options, e.g. --release]".into());
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned();
    let mut child = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root)
        .args(["build", "--package", "vk-color-bench", "--lib"])
        .args(args)
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .spawn()?;
    let mut out_dir = None;
    let mut library = None;
    let mut package_id = None;
    let mut outputs = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            eprintln!("{line}");
            continue;
        };
        match message["reason"].as_str() {
            Some("compiler-message") => {
                if let Some(rendered) = message["message"]["rendered"].as_str() {
                    eprint!("{rendered}");
                }
            }
            Some("build-script-executed") => {
                if let (Some(id), Some(path)) =
                    (message["package_id"].as_str(), message["out_dir"].as_str())
                {
                    outputs.push((id.to_owned(), PathBuf::from(path)));
                }
            }
            Some("compiler-artifact")
                if message["target"]["crate_types"]
                    .as_array()
                    .is_some_and(|types| types.iter().any(|kind| kind == "cdylib")) =>
            {
                package_id = message["package_id"].as_str().map(str::to_owned);
                library = message["filenames"].as_array().and_then(|files| {
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from)
                        .find(|path| {
                            matches!(
                                path.extension().and_then(|extension| extension.to_str()),
                                Some("dll" | "so" | "dylib")
                            )
                        })
                });
            }
            _ => {}
        }
    }
    if !child.wait()?.success() {
        return Err("layer build failed; manifest was not packaged".into());
    }
    for (id, path) in outputs {
        if Some(&id) == package_id.as_ref() {
            out_dir = Some(path);
        }
    }
    let library = library.ok_or("Cargo did not report a layer shared library")?;
    let source = out_dir
        .ok_or("Cargo did not report the layer build-script output")?
        .join("layer_manifest.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&source)?)?;
    let name = manifest["layer"]["name"]
        .as_str()
        .ok_or("manifest has no layer name")?;
    if name.contains(['/', '\\']) {
        return Err("invalid layer name".into());
    }
    let destination = library.parent().unwrap().join(format!("{name}.json"));
    fs::copy(source, &destination)?;
    println!("Packaged {}", library.display());
    println!("Manifest {}", destination.display());
    Ok(())
}
