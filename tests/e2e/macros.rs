/// Generate a #[tokio::test] for each case directory
#[macro_export]
macro_rules! e2e_test_cases {
    ($dir:expr, $( $name:ident => $folder:expr ),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                let case_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join($dir)
                    .join($folder);
                crate::e2e::runner::run_case(&case_dir).await;
            }
        )*
    };
}
