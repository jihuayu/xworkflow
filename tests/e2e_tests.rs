mod e2e;

e2e::e2e_test_cases!(
    "tests/e2e/cases",
    case_001_simple_passthrough => "001_simple_passthrough",
    case_002_ifelse_true_branch => "002_ifelse_true_branch",
    case_003_template_transform => "003_template_transform",
);
