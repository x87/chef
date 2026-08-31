fn main() {
    // Windows-only: embeds the exe icon/version resource. No-op elsewhere so
    // Linux/other targets can build without winres or a cross resource compiler.
    #[cfg(windows)]
    {
        let res = winres::WindowsResource::new();
        res.compile().unwrap();
    }
}
