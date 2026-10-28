//! On Windows, gives demirror.exe its icon and version details.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/demirror.ico");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if windows && msvc {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../packaging/icons/demirror.ico");
        resource.set("FileDescription", "demirror");
        resource.set("ProductName", "demirror");
        resource.compile().expect("compiling demirror's Windows resources");
    }
}
