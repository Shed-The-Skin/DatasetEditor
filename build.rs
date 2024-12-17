
#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico"); // This path is relative to your project root
    res.compile().unwrap();
}

#[cfg(not(windows))]
fn main() {}