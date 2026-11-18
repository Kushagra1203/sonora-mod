//! The graphics stack the app draws with, and what it leaves out of the process.

/// The directories the Vulkan loader reads its driver manifests from.
const MANIFESTS: [&str; 2] = ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"];

/// The drivers that draw in software. Their libraries carry a compiler in, which is a
/// third of the memory the app holds, and they are never the one that draws.
const SOFTWARE: [&str; 4] = ["lvp", "lavapipe", "swiftshader", "llvmpipe"];

/// Keeps the software Vulkan driver out of the process.
///
/// The loader opens every driver it is told about, and the software one brings a
/// compiler along that is worth more memory than the app's own images. The hardware
/// drivers stay, so the app still draws on the GPU. A machine with no hardware driver
/// keeps every driver it has.
#[cfg(target_os = "linux")]
pub fn keep_hardware_vulkan() {
    let mut hardware = Vec::new();
    for directory in MANIFESTS {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let Ok(manifest) = std::fs::read_to_string(&path) else {
                continue;
            };
            if SOFTWARE.iter().any(|driver| manifest.contains(driver)) {
                continue;
            }
            hardware.push(path);
        }
    }

    if hardware.is_empty() {
        return;
    }

    let drivers = hardware
        .iter()
        .filter_map(|path| path.to_str())
        .collect::<Vec<_>>()
        .join(":");
    // SAFETY: the app has no threads yet, and nothing else reads the environment.
    unsafe {
        std::env::set_var("VK_DRIVER_FILES", &drivers);
        std::env::set_var("VK_ICD_FILENAMES", &drivers);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn keep_hardware_vulkan() {}
