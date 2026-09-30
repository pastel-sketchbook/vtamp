fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // Share identity with the automatically prepared macOS playback-server bundle.
    let plist = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("Info.plist");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    std::fs::write(&plist, format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.xrath.vtamp</string>
<key>CFBundleName</key><string>vtamp</string>
<key>CFBundleDisplayName</key><string>vtamp</string>
<key>CFBundleExecutable</key><string>vtamp</string>
<key>CFBundleIconFile</key><string>vtamp.icns</string>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>CFBundleVersion</key><string>{version}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSUIElement</key><true/>
</dict></plist>"#)).unwrap();
    for arg in ["-sectcreate", "__TEXT", "__info_plist"] {
        println!("cargo:rustc-link-arg-bin=vtamp=-Wl,{arg}");
    }
    println!("cargo:rustc-link-arg-bin=vtamp={}", plist.display());
}
