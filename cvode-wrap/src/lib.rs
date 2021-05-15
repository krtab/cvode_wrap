//! A wrapper around the sundials ODE solver.

use std::{convert::TryInto, pin::Pin};
use std::{ffi::c_void, intrinsics::transmute, os::raw::c_int, ptr::NonNull};

use cvode_5_sys::{
    realtype, SUNLinearSolver, SUNMatrix,
    N_VGetArrayPointer,
};

mod nvector;
pub use nvector::{NVectorSerial, NVectorSerialHeapAlloced};

pub type Realtype = realtype;

#[repr(u32)]
#[derive(Debug)]
pub enum LinearMultistepMethod {
    ADAMS = cvode_5_sys::CV_ADAMS,
    BDF = cvode_5_sys::CV_BDF,
}

#[repr(C)]
struct CvodeMemoryBlock {
    _private: [u8; 0],
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct CvodeMemoryBlockNonNullPtr {
    ptr: NonNull<CvodeMemoryBlock>,
}

impl CvodeMemoryBlockNonNullPtr {
    fn new(ptr: NonNull<CvodeMemoryBlock>) -> Self {
        Self { ptr }
    }

    fn as_raw(self) -> *mut c_void {
        self.ptr.as_ptr() as *mut c_void
    }
}

impl From<NonNull<CvodeMemoryBlock>> for CvodeMemoryBlockNonNullPtr {
    fn from(x: NonNull<CvodeMemoryBlock>) -> Self {
        Self::new(x)
    }
}

/// The main struct of the crate. Wraps a sundials solver.
///
/// Args
/// ----
/// `UserData` is the type of the supplementary arguments for the
/// right-hand-side. If unused, should be `()`.
///
/// `N` is the "problem size", that is the dimension of the state space.
///
/// See [crate-level](`crate`) documentation for more.
pub struct Solver<UserData, const N: usize> {
    mem: CvodeMemoryBlockNonNullPtr,
    y0: NVectorSerialHeapAlloced<N>,
    sunmatrix: SUNMatrix,
    linsolver: SUNLinearSolver,
    atol: AbsTolerance<N>,
    user_data: Pin<Box<UserData>>,
}

pub enum RhsResult {
    Ok,
    RecoverableError(u8),
    NonRecoverableError(u8),
}

pub type RhsF<UserData, const N: usize> =
    fn(t: Realtype, y: &[Realtype; N], ydot: &mut [Realtype; N], user_data: &UserData) -> RhsResult;

pub type RhsFCtype<UserData, const N: usize> = extern "C" fn(
    t: Realtype,
    y: *const NVectorSerial<N>,
    ydot: *mut NVectorSerial<N>,
    user_data: *const UserData,
) -> c_int;

pub fn wrap_f<UserData, const N: usize>(
    f: RhsF<UserData, N>,
    t: Realtype,
    y: *const NVectorSerial<N>,
    ydot: *mut NVectorSerial<N>,
    data: &UserData,
) -> c_int {
    let y = unsafe { transmute(N_VGetArrayPointer(y as _)) };
    let ydot = unsafe { transmute(N_VGetArrayPointer(ydot as _)) };
    let res = f(t, y, ydot, data);
    match res {
        RhsResult::Ok => 0,
        RhsResult::RecoverableError(e) => e as c_int,
        RhsResult::NonRecoverableError(e) => -(e as c_int),
    }
}

#[macro_export]
macro_rules! wrap {
    ($wrapped_f_name: ident, $f_name: ident, $user_data: ty, $problem_size: expr) => {
        extern "C" fn $wrapped_f_name(
            t: Realtype,
            y: *const NVectorSerial<$problem_size>,
            ydot: *mut NVectorSerial<$problem_size>,
            data: *const $user_data,
        ) -> std::os::raw::c_int {
            let data = unsafe { std::mem::transmute(data) };
            wrap_f($f_name, t, y, ydot, data)
        }
    };
}

#[repr(u32)]
pub enum StepKind {
    Normal = cvode_5_sys::CV_NORMAL,
    OneStep = cvode_5_sys::CV_ONE_STEP,
}

#[derive(Debug)]
pub enum Error {
    NullPointerError { func_id: &'static str },
    ErrorCode { func_id: &'static str, flag: c_int },
}

pub type Result<T> = std::result::Result<T, Error>;

fn check_non_null<T>(ptr: *mut T, func_id: &'static str) -> Result<NonNull<T>> {
    NonNull::new(ptr).ok_or_else(|| Error::NullPointerError { func_id })
}

fn check_flag_is_succes(flag: c_int, func_id: &'static str) -> Result<()> {
    if flag == cvode_5_sys::CV_SUCCESS as i32 {
        Ok(())
    } else {
        Err(Error::ErrorCode { flag, func_id })
    }
}

pub enum AbsTolerance<const SIZE: usize> {
    Scalar(Realtype),
    Vector(NVectorSerialHeapAlloced<SIZE>),
}

impl<const SIZE: usize> AbsTolerance<SIZE> {
    pub fn scalar(atol: Realtype) -> Self {
        AbsTolerance::Scalar(atol)
    }

    pub fn vector(atol: &[Realtype; SIZE]) -> Self {
        let atol = NVectorSerialHeapAlloced::new_from(atol);
        AbsTolerance::Vector(atol)
    }
}

impl<UserData, const N: usize> Solver<UserData, N> {
    pub fn new(
        method: LinearMultistepMethod,
        f: RhsFCtype<UserData, N>,
        t0: Realtype,
        y0: &[Realtype; N],
        rtol: Realtype,
        atol: AbsTolerance<N>,
        user_data: UserData,
    ) -> Result<Self> {
        assert_eq!(y0.len(), N);
        let mem: CvodeMemoryBlockNonNullPtr = {
            let mem_maybenull = unsafe { cvode_5_sys::CVodeCreate(method as c_int) };
            check_non_null(mem_maybenull as *mut CvodeMemoryBlock, "CVodeCreate")?.into()
        };
        let y0 = NVectorSerialHeapAlloced::new_from(y0);
        let matrix = {
            let matrix = unsafe {
                cvode_5_sys::SUNDenseMatrix(
                    N.try_into().unwrap(),
                    N.try_into().unwrap(),
                )
            };
            check_non_null(matrix, "SUNDenseMatrix")?
        };
        let linsolver = {
            let linsolver = unsafe {
                cvode_5_sys::SUNDenseLinearSolver(
                    y0.as_raw(),
                    matrix.as_ptr(),
                )
            };
            check_non_null(linsolver, "SUNDenseLinearSolver")?
        };
        let user_data = Box::pin(user_data);
        let res = Solver {
            mem,
            y0,
            sunmatrix: matrix.as_ptr(),
            linsolver: linsolver.as_ptr(),
            atol,
            user_data,
        };
        {
            let flag = unsafe {
                cvode_5_sys::CVodeInit(
                    mem.as_raw(),
                    Some(std::mem::transmute(f)),
                    t0,
                    res.y0.as_raw(),
                )
            };
            check_flag_is_succes(flag, "CVodeInit")?;
        }
        match &res.atol {
            &AbsTolerance::Scalar(atol) => {
                let flag = unsafe { cvode_5_sys::CVodeSStolerances(mem.as_raw(), rtol, atol) };
                check_flag_is_succes(flag, "CVodeSStolerances")?;
            }
            AbsTolerance::Vector(atol) => {
                let flag =
                    unsafe { cvode_5_sys::CVodeSVtolerances(mem.as_raw(), rtol, atol.as_raw()) };
                check_flag_is_succes(flag, "CVodeSVtolerances")?;
            }
        }
        {
            let flag = unsafe {
                cvode_5_sys::CVodeSetLinearSolver(
                    mem.as_raw(),
                    linsolver.as_ptr(),
                    matrix.as_ptr(),
                )
            };
            check_flag_is_succes(flag, "CVodeSetLinearSolver")?;
        }
        {
            let flag = unsafe {
                cvode_5_sys::CVodeSetUserData(
                    mem.as_raw(),
                    std::mem::transmute(res.user_data.as_ref().get_ref()),
                )
            };
            check_flag_is_succes(flag, "CVodeSetUserData")?;
        }
        Ok(res)
    }

    pub fn step(
        &mut self,
        tout: Realtype,
        step_kind: StepKind,
    ) -> Result<(Realtype, &[Realtype; N])> {
        let mut tret = 0.;
        let flag = unsafe {
            cvode_5_sys::CVode(
                self.mem.as_raw(),
                tout,
                self.y0.as_raw(),
                &mut tret,
                step_kind as c_int,
            )
        };
        check_flag_is_succes(flag, "CVode")?;
        Ok((tret, self.y0.as_slice()))
    }
}

impl<UserData, const N: usize> Drop for Solver<UserData, N> {
    fn drop(&mut self) {
        unsafe { cvode_5_sys::CVodeFree(&mut self.mem.as_raw()) }
        unsafe { cvode_5_sys::SUNLinSolFree(self.linsolver) };
        unsafe { cvode_5_sys::SUNMatDestroy(self.sunmatrix) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(
        _t: super::Realtype,
        y: &[Realtype; 2],
        ydot: &mut [Realtype; 2],
        _data: &(),
    ) -> RhsResult {
        *ydot = [y[1], -y[0]];
        RhsResult::Ok
    }

    wrap!(wrapped_f, f, (), 2);

    #[test]
    fn create() {
        let y0 = [0., 1.];
        let _solver = Solver::new(
            LinearMultistepMethod::ADAMS,
            wrapped_f,
            0.,
            &y0,
            1e-4,
            AbsTolerance::Scalar(1e-4),
            (),
        );
    }
}
