use _rust_il::callbacks::CancelFlag;
use _rust_il::term_extractor::optional_extraction_result;
use pyo3::{exceptions::{PyKeyboardInterrupt, PyRuntimeError}, prelude::*};

#[test]
fn optional_stage_failures_fall_back_and_cancellations_propagate() {
    Python::initialize();
    Python::attach(|py| {
        let flag = CancelFlag::default();
        assert!(optional_extraction_result(Err(PyRuntimeError::new_err("offline failure")), &flag).is_ok());
        assert!(optional_extraction_result(Err(PyKeyboardInterrupt::new_err(())), &flag)
            .unwrap_err().is_instance_of::<PyKeyboardInterrupt>(py));
        for module in ["asyncio", "concurrent.futures"] {
            let error = PyErr::from_value(py.import(module).unwrap().getattr("CancelledError").unwrap().call0().unwrap());
            assert!(optional_extraction_result(Err(error), &flag).is_err(), "{module}");
        }
        flag.cancel();
        let error = optional_extraction_result(Ok(()), &flag).unwrap_err();
        let class = py.import("asyncio").unwrap().getattr("CancelledError").unwrap();
        assert!(error.is_instance(py, &class));
    });
}
