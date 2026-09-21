//! Attaches the application icon and version details to the executable.
//!
//! Windows reads these from the binary itself, not from the installer, so
//! this is what gives Rhevia its icon in Explorer, on the taskbar and on the
//! Start Menu shortcut. Without it the shortcut shows a blank default.

fn main() {
    // Only Windows carries resources in the executable; everywhere else this
    // is a no-op and the build must not fail because of it.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../assets/rhevia.ico");

        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/rhevia.ico");
        resource.set("ProductName", "Rhevia Studio");
        resource.set("FileDescription", "Rhevia Studio — live production switcher");
        resource.set("CompanyName", "Rhevia");
        resource.set("LegalCopyright", "Copyright Rhevia");

        if let Err(e) = resource.compile() {
            // A missing resource compiler must not stop someone building the
            // program; it only costs the icon.
            println!("cargo:warning=could not attach the icon: {e}");
        }
    }
}
