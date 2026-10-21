//! A .cvimodel on the NPU through libcviruntime (sophgo-cviruntime).

use crate::Error;
use sophgo_sys as ffi;
use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::path::Path;

/// A loaded model with its input and output tensors.
pub struct Model {
    handle: ffi::CVI_MODEL_HANDLE,
    inputs: *mut ffi::CVI_TENSOR,
    nin: i32,
    outputs: *mut ffi::CVI_TENSOR,
    nout: i32,
}

// The runtime handle is used by one thread at a time (&mut for forward).
unsafe impl Send for Model {}

impl Model {
    pub fn open(path: &Path) -> Result<Model, Error> {
        let p = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| Error { op: "CVI_NN_RegisterModel (path)", code: -1 })?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: p is a valid C string; handle an out-parameter
        let r = unsafe { ffi::CVI_NN_RegisterModel(p.as_ptr(), &mut handle) };
        if r != 0 || handle.is_null() {
            return Err(Error { op: "CVI_NN_RegisterModel", code: r });
        }
        let mut m = Model { handle, inputs: std::ptr::null_mut(), nin: 0, outputs: std::ptr::null_mut(), nout: 0 };
        // SAFETY: out-parameters; the arrays belong to the model
        let r = unsafe {
            ffi::CVI_NN_GetInputOutputTensors(m.handle, &mut m.inputs, &mut m.nin, &mut m.outputs, &mut m.nout)
        };
        if r != 0 || m.inputs.is_null() || m.outputs.is_null() {
            return Err(Error { op: "CVI_NN_GetInputOutputTensors", code: r });
        }
        Ok(m)
    }

    pub fn inputs(&self) -> usize {
        self.nin as usize
    }

    pub fn outputs(&self) -> usize {
        self.nout as usize
    }

    pub fn input(&self, i: usize) -> Tensor<'_> {
        assert!(i < self.inputs());
        // SAFETY: i is within the array the runtime gave
        Tensor { t: unsafe { self.inputs.add(i) }, _m: PhantomData }
    }

    pub fn output(&self, i: usize) -> Tensor<'_> {
        assert!(i < self.outputs());
        // SAFETY: i is within the array the runtime gave
        Tensor { t: unsafe { self.outputs.add(i) }, _m: PhantomData }
    }

    /// Feed input 0 from a physical address (a VPSS frame: no copy).
    pub fn set_input_phys(&mut self, paddr: u64) -> Result<(), Error> {
        // SAFETY: input 0 exists (checked at open by its users)
        let r = unsafe { ffi::CVI_NN_SetTensorPhysicalAddr(self.inputs, paddr) };
        if r != 0 { Err(Error { op: "CVI_NN_SetTensorPhysicalAddr", code: r }) } else { Ok(()) }
    }

    /// The runtime's own buffer of input 0, to be filled by the CPU.
    pub fn input_buffer(&mut self) -> &mut [u8] {
        // SAFETY: the runtime's buffer of input 0, TensorSize bytes, owned
        // by the model and borrowed mutably here
        unsafe {
            let p = ffi::CVI_NN_TensorPtr(self.inputs) as *mut u8;
            std::slice::from_raw_parts_mut(p, ffi::CVI_NN_TensorSize(self.inputs))
        }
    }

    /// One blocking inference.
    pub fn forward(&mut self) -> Result<(), Error> {
        // SAFETY: the tensor arrays of this model
        let r = unsafe { ffi::CVI_NN_Forward(self.handle, self.inputs, self.nin, self.outputs, self.nout) };
        if r != 0 { Err(Error { op: "CVI_NN_Forward", code: r }) } else { Ok(()) }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: registered by open; the tensors borrowed from it are gone
        unsafe { ffi::CVI_NN_CleanupModel(self.handle) };
    }
}

/// A tensor of a model (borrowed from it).
pub struct Tensor<'m> {
    t: *mut ffi::CVI_TENSOR,
    _m: PhantomData<&'m ()>,
}

impl Tensor<'_> {
    pub fn name(&self) -> String {
        // SAFETY: the runtime keeps the name for the model's lifetime
        unsafe {
            let p = ffi::CVI_NN_TensorName(self.t);
            if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
        }
    }

    /// The dimensions in use (NCHW for the outputs, NHWC for an RGB input).
    pub fn shape(&self) -> Vec<i32> {
        // SAFETY: plain accessor
        let s = unsafe { ffi::CVI_NN_TensorShape(self.t) };
        s.dim[..s.dim_size.min(ffi::CVI_DIM_MAX)].to_vec()
    }

    /// real value = int8 value * qscale
    pub fn qscale(&self) -> f32 {
        // SAFETY: plain accessor
        unsafe { ffi::CVI_NN_TensorQuantScale(self.t) }
    }

    pub fn size(&self) -> usize {
        // SAFETY: plain accessor
        unsafe { ffi::CVI_NN_TensorSize(self.t) }
    }

    /// The data as f32 (a float output), valid after forward.
    pub fn data_f32(&self) -> &[f32] {
        // SAFETY: the runtime's buffer of this tensor, size bytes, aligned
        // for its element type
        unsafe { std::slice::from_raw_parts(ffi::CVI_NN_TensorPtr(self.t) as *const f32, self.size() / 4) }
    }

    /// The data as INT8 (the outputs of an int8 model), valid after forward.
    pub fn data_i8(&self) -> &[i8] {
        // SAFETY: the runtime's buffer of this tensor, size bytes
        unsafe { std::slice::from_raw_parts(ffi::CVI_NN_TensorPtr(self.t) as *const i8, self.size()) }
    }
}
