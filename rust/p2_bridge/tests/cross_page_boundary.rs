#![allow(dead_code)]

mod ordinary_translation {
    use babeldoc_p2_stages::translation::ordinary_translation::{PlaceholderRegex, RegexMatch, TranslateInput};
    use pyo3::prelude::*;

    #[derive(Clone)]
    pub(crate) struct Job {
        pub page: usize,
        pub paragraph: usize,
        pub eligible: bool,
        pub vertical: bool,
        pub missing_font_style: Option<()>,
        pub input: Option<TranslateInput>,
        pub token_count: usize,
    }

    pub(crate) struct RegexService<'py> {
        pub service: Bound<'py, PyAny>,
    }

    impl PlaceholderRegex for RegexService<'_> {
        type Error = PyErr;

        fn compile(&mut self, _: &str, _: bool) -> PyResult<u64> { unreachable!() }
        fn matches(&mut self, _: u64, _: &str) -> PyResult<bool> { unreachable!() }
        fn find_all(&mut self, _: u64, _: &str) -> PyResult<Vec<RegexMatch>> { unreachable!() }
    }
}

#[path = "../src/cross_page.rs"]
mod cross_page;
