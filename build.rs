use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    let compiler = env::var_os("VULKAN_SDK")
        .map(|sdk| {
            PathBuf::from(sdk).join(if cfg!(windows) {
                "Bin/glslc.exe"
            } else {
                "bin/glslc"
            })
        })
        .unwrap_or_else(|| PathBuf::from("glslc"));
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    generate_vulkan_layer_manifest(&out);

    for shader in [
        "fullscreen.vert",
        "clipping.frag",
        "channels.frag",
        "gamut.frag",
        "luminance.frag",
    ] {
        let source = format!("shaders/{shader}");
        println!("cargo:rerun-if-changed={source}");
        let status = Command::new(&compiler)
            .args(["--target-env=vulkan1.0", "-O", &source, "-o"])
            .arg(out.join(format!("{shader}.spv")))
            .status()
            .expect("could not run glslc; install the Vulkan SDK and set VULKAN_SDK or PATH");
        assert!(status.success(), "shader compilation failed: {source}");
    }

    println!("cargo:rerun-if-changed=Cargo.toml");
}

#[derive(Serialize)]
struct LayerManifestFile {
    file_format_version: String,
    layer: LayerManifest,
}

#[derive(Serialize)]
struct LayerManifest {
    name: String,

    #[serde(rename = "type")]
    layer_type: LayerType,

    library_path: String,

    api_version: String,
    implementation_version: String,
    description: String,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum LayerType {
    Global,
    Instance,
}

#[derive(Deserialize)]
struct CargoManifest {
    package: Package,
    lib: Library,
}

#[derive(Deserialize)]
struct Package {
    version: String,
    description: String,
    metadata: Metadata,
}

#[derive(Deserialize)]
struct Library {
    name: String,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(rename = "vulkan-layer")]
    vulkan_layer: VulkanLayerMetadata,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct VulkanLayerMetadata {
    name: String,

    #[serde(rename = "type")]
    layer_type: LayerType,

    api_version: String,
}

fn generate_vulkan_layer_manifest(out_dir: &Path) {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source =
        fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("failed to read Cargo.toml");

    let cargo: CargoManifest = toml::from_str(&source).expect("invalid Cargo.toml layer metadata");

    let layer = &cargo.package.metadata.vulkan_layer;

    let parse_version = |version: &str| {
        let parts: Vec<u32> = version
            .split('.')
            .map(|part| part.parse().expect("version must be major.minor.patch"))
            .collect();
        assert!(
            parts.len() == 3 && parts[0] < 128 && parts[1] < 1024 && parts[2] < 4096,
            "version exceeds Vulkan version encoding limits"
        );
        (parts[0] << 22) | (parts[1] << 12) | parts[2]
    };
    let implementation_version = parse_version(&cargo.package.version);
    let api_version = parse_version(&layer.api_version);
    for text in [&layer.name, &cargo.package.description] {
        assert!(
            text.len() < 256 && !text.contains('\0'),
            "layer name/description must fit Vulkan's 256-byte strings"
        );
    }
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let library_name = match target_os.as_str() {
        "windows" => format!("{}.dll", cargo.lib.name),
        "macos" => format!("lib{}.dylib", cargo.lib.name),
        _ => format!("lib{}.so", cargo.lib.name),
    };
    let library_path = if target_os == "windows" {
        format!(".\\{library_name}")
    } else {
        format!("./{library_name}")
    };
    fs::write(out_dir.join("layer_metadata.rs"), format!(
        "pub const NAME: &str = {:?};\npub const DESCRIPTION: &str = {:?};\npub const API_VERSION: u32 = {};\npub const IMPLEMENTATION_VERSION: u32 = {};\n",
        layer.name, cargo.package.description, api_version, implementation_version,
    )).expect("failed to write layer metadata");

    let manifest = LayerManifest {
        name: layer.name.clone(),
        layer_type: layer.layer_type,
        library_path,
        api_version: layer.api_version.clone(),
        implementation_version: implementation_version.to_string(),
        description: cargo.package.description.clone(),
    };

    let json = serde_json::to_string_pretty(&LayerManifestFile {
        file_format_version: "1.2.0".to_string(),
        layer: manifest,
    })
    .unwrap();
    std::fs::write(out_dir.join("layer_manifest.json"), json).unwrap();
}
