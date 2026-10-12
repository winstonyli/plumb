//! Direct3D 11 device, Direct2D device context and DirectWrite factory.
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::core::{Interface, Result};

pub struct Gfx {
    pub d3d: ID3D11Device,
    pub d2d: ID2D1DeviceContext,
    pub dw: IDWriteFactory,
    /// "AMD Radeon RX 9060 XT (hardware)" or "Microsoft Basic Render Driver (WARP)".
    pub adapter: String,
}

impl Gfx {
    /// Software device: no window, no physical GPU. For tests.
    pub fn warp() -> Result<Self> {
        Self::create(None, true)
    }

    /// Hardware device on `adapter`, or the system default when `None`.
    pub fn hardware(adapter: Option<&IDXGIAdapter>) -> Result<Self> {
        Self::create(adapter, false)
    }

    fn create(adapter: Option<&IDXGIAdapter>, warp: bool) -> Result<Self> {
        let mut d3d = None;
        let kind = if warp {
            D3D_DRIVER_TYPE_WARP
        } else if adapter.is_some() {
            D3D_DRIVER_TYPE_UNKNOWN
        } else {
            D3D_DRIVER_TYPE_HARDWARE
        };
        unsafe {
            D3D11CreateDevice(
                adapter,
                kind,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
        }
        let d3d = d3d.expect("D3D11CreateDevice succeeded without a device");
        let dxgi: IDXGIDevice = d3d.cast()?;
        let desc = unsafe { dxgi.GetAdapter()?.GetDesc()? };
        let n = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let adapter = format!(
            "{} ({})",
            String::from_utf16_lossy(&desc.Description[..n]),
            if warp { "WARP" } else { "hardware" }
        );
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let d2d_dev = unsafe { factory.CreateDevice(&dxgi)? };
        let d2d = unsafe { d2d_dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self { d3d, d2d, dw, adapter })
    }
}
