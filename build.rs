fn main() {
    // Give the Windows executable its icon.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico");
        res.compile().expect("could not embed the Windows icon");
    }
}
