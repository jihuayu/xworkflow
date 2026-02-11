/// Generate a #[tokio::test] for each case directory
#[macro_export]
macro_rules! integration_test_cases {
    ($dir:expr, $( $name:ident => $folder:expr ),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                let case_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join($dir)
                    .join($folder);
                crate::integration::runner::run_case(&case_dir).await;
            }
        )*
    };
}

/// Generate a #[tokio::test] for each debug-mode case directory
#[macro_export]
macro_rules! integration_debug_test_cases {
    ($dir:expr, $( $name:ident => $folder:expr ),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                let case_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join($dir)
                    .join($folder);
                crate::integration::runner::run_debug_case(&case_dir).await;
            }
        )*
    };
}
