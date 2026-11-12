use std::process::Command;

fn main() {
    // 1. 把 res.o（manifest + ico）编译出来
    let windres = r"C:\msys64\mingw64\bin\windres.exe";
    let rc = "res.rc";
    let status = Command::new(windres)
        .args([rc, "-O", "coff", "-o", "res.o"])
        .current_dir(".")
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("windres failed: {s}"),
        Err(e) => panic!("windres: {e}"),
    }

    // 2. 链接参数
    //    - res.o 把 manifest + icon 嵌入 exe
    //    - GUI 子系统：不弹控制台（静默后台）。mingw 用 -Wl,-subsystem,windows
    println!("cargo:rustc-link-arg=res.o");
    println!("cargo:rustc-link-arg=-Wl,-subsystem,windows");

    println!("cargo:rerun-if-changed={}", rc);
    println!("cargo:rerun-if-changed=res.o");
    println!("cargo:rerun-if-changed=keyguard.manifest");
    println!("cargo:rerun-if-changed=keyguard.ico");
}
