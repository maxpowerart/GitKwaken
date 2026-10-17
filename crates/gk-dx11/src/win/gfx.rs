//! Direct3D 11 device, DXGI swap chain and the egui renderer on top of them.

use anyhow::{Context as _, Result, anyhow};
use windows::Win32::Foundation::{DXGI_STATUS_OCCLUDED, E_INVALIDARG, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

/// Non-sRGB back buffer: egui-directx11 blends in gamma space.
const FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;

const FEATURE_LEVELS: [D3D_FEATURE_LEVEL; 4] = [
    D3D_FEATURE_LEVEL_11_1,
    D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_10_0,
];

/// Largest texture side we advertise to egui (D3D 10.0 guarantees 8192).
pub const MAX_TEXTURE_SIDE: usize = 8192;

pub enum Presented {
    Shown,
    /// Window fully hidden; caller should throttle.
    Occluded,
}

pub struct Gfx {
    // Field order is drop order: views and renderer go before the swap chain.
    renderer: egui_directx11::Renderer,
    rtv: Option<ID3D11RenderTargetView>,
    swap_chain: IDXGISwapChain1,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    size: [u32; 2],
    /// Human-readable adapter / driver / feature level summary.
    pub info: String,
}

impl Gfx {
    pub fn new(hwnd: HWND, size: [u32; 2]) -> Result<Self> {
        let (device, context, driver) = create_device()?;
        let dxgi_device: IDXGIDevice1 = device.cast()?;
        // One queued frame keeps input latency low; failure is harmless.
        let _ = unsafe { dxgi_device.SetMaximumFrameLatency(1) };
        let adapter = unsafe { dxgi_device.GetAdapter() }?;
        // Must be the factory that owns the device's adapter.
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }?;
        let (swap_chain, flip) = create_swap_chain(&factory, &device, hwnd, size)?;
        let _ = unsafe { factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER) };
        let renderer =
            egui_directx11::Renderer::new(&device).context("creating egui D3D11 renderer")?;

        let level = unsafe { device.GetFeatureLevel() };
        let info = format!(
            "Direct3D 11 | {} | {} | feature level {} | {} swap chain",
            adapter_name(&adapter),
            if driver == D3D_DRIVER_TYPE_WARP { "WARP (software)" } else { "hardware" },
            level_name(level),
            if flip { "flip-discard" } else { "legacy discard" },
        );
        log::info!("{info}");

        Ok(Self {
            renderer,
            rtv: None,
            swap_chain,
            context,
            device,
            size: [size[0].max(1), size[1].max(1)],
            info,
        })
    }

    /// Clear, draw egui and present one frame at `size` (physical pixels).
    pub fn frame(
        &mut self,
        ctx: &egui::Context,
        output: egui_directx11::RendererOutput,
        clear: [f32; 4],
        size: [u32; 2],
        vsync: bool,
    ) -> windows::core::Result<Presented> {
        if size != self.size {
            self.resize(size)?;
        }
        let rtv = match &self.rtv {
            Some(rtv) => rtv.clone(),
            None => {
                let back: ID3D11Texture2D = unsafe { self.swap_chain.GetBuffer(0) }?;
                let mut rtv = None;
                unsafe { self.device.CreateRenderTargetView(&back, None, Some(&mut rtv)) }?;
                let rtv = rtv.ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
                self.rtv = Some(rtv.clone());
                rtv
            }
        };
        unsafe { self.context.ClearRenderTargetView(&rtv, &clear) };
        self.renderer.render(&self.context, &rtv, ctx, output)?;

        let hr = unsafe { self.swap_chain.Present(u32::from(vsync), DXGI_PRESENT(0)) };
        if hr == DXGI_STATUS_OCCLUDED {
            Ok(Presented::Occluded)
        } else {
            hr.ok().map(|()| Presented::Shown)
        }
    }

    fn resize(&mut self, size: [u32; 2]) -> windows::core::Result<()> {
        // Every reference to the back buffer must be gone before ResizeBuffers.
        self.rtv = None;
        unsafe {
            self.context.ClearState();
            self.context.Flush();
            self.swap_chain.ResizeBuffers(
                0,
                size[0],
                size[1],
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        self.size = size;
        Ok(())
    }
}

/// Hardware first, then WARP (VMs, RDP, broken drivers).
/// `GK_DX11_WARP=1` forces WARP for testing.
fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, D3D_DRIVER_TYPE)> {
    let force_warp = std::env::var_os("GK_DX11_WARP").is_some_and(|v| v != "0");
    let drivers: &[D3D_DRIVER_TYPE] = if force_warp {
        &[D3D_DRIVER_TYPE_WARP]
    } else {
        &[D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP]
    };
    let mut errors = Vec::new();
    for &driver in drivers {
        // Runtimes without 11.1 reject the whole list with E_INVALIDARG.
        for levels in [&FEATURE_LEVELS[..], &FEATURE_LEVELS[1..]] {
            let (mut device, mut context) = (None, None);
            let res = unsafe {
                D3D11CreateDevice(
                    None::<&IDXGIAdapter>,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (res, device, context) {
                (Ok(()), Some(device), Some(context)) => return Ok((device, context, driver)),
                (Err(e), ..) if e.code() == E_INVALIDARG => errors.push(e),
                (Err(e), ..) => {
                    errors.push(e);
                    break;
                }
                (Ok(()), ..) => break,
            }
        }
        log::warn!("D3D11 device creation failed for driver type {}", driver.0);
    }
    Err(anyhow!("could not create a Direct3D 11 device: {errors:?}"))
}

/// Flip-discard first (Win10+), legacy blt-model discard as fallback.
fn create_swap_chain(
    factory: &IDXGIFactory2,
    device: &ID3D11Device,
    hwnd: HWND,
    size: [u32; 2],
) -> Result<(IDXGISwapChain1, bool)> {
    let mut desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: size[0].max(1),
        Height: size[1].max(1),
        Format: FORMAT,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
        Flags: 0,
    };
    match unsafe { factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None) } {
        Ok(sc) => return Ok((sc, true)),
        Err(e) => log::warn!("flip-model swap chain unavailable ({e}); using legacy discard"),
    }
    desc.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
    desc.BufferCount = 1;
    let sc = unsafe { factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None) }
        .context("creating DXGI swap chain")?;
    Ok((sc, false))
}

fn adapter_name(adapter: &IDXGIAdapter) -> String {
    match unsafe { adapter.GetDesc() } {
        Ok(desc) => {
            let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            String::from_utf16_lossy(&desc.Description[..len])
        }
        Err(_) => "unknown adapter".to_owned(),
    }
}

fn level_name(level: D3D_FEATURE_LEVEL) -> &'static str {
    match level {
        D3D_FEATURE_LEVEL_11_1 => "11_1",
        D3D_FEATURE_LEVEL_11_0 => "11_0",
        D3D_FEATURE_LEVEL_10_1 => "10_1",
        D3D_FEATURE_LEVEL_10_0 => "10_0",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_level_names() {
        assert_eq!(level_name(D3D_FEATURE_LEVEL_11_1), "11_1");
        assert_eq!(level_name(D3D_FEATURE_LEVEL_10_0), "10_0");
        assert_eq!(level_name(D3D_FEATURE_LEVEL_9_3), "other");
    }

    /// Headless: WARP is always present on Windows 10+.
    #[test]
    fn warp_device_is_creatable() {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&FEATURE_LEVELS[1..]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .unwrap();
        let device = device.unwrap();
        assert!(egui_directx11::Renderer::new(&device).is_ok());
    }
}
