use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/yunta.rc");
    println!("cargo:rerun-if-changed=assets/yunta.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let resource;
    let status = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        resource = out.join("yunta-icon.o");
        let target = env::var("TARGET").unwrap();
        let tool = if env::var("HOST").unwrap().contains("windows") {
            "windres".to_string()
        } else {
            format!("{}-w64-mingw32-windres", target.split('-').next().unwrap())
        };
        Command::new(tool).args(["-i", "assets/yunta.rc", "-O", "coff", "-o"]).arg(&resource).status()
    } else {
        resource = out.join("yunta-icon.res");
        Command::new("rc.exe").arg("/nologo").arg(format!("/fo{}", resource.display())).arg("assets/yunta.rc").status()
    };
    assert!(status.expect("Windows resource compiler is required to embed the Yunta icon").success(), "could not compile Yunta icon");
    println!("cargo:rustc-link-arg={}", resource.display());
}
