//! Rendu GPU : D3D11 + Direct2D (sprite batch) dans une swapchain
//! DirectComposition à alpha prémultiplié. Un seul appel de dessin par image.

use crate::sprites::{self, Sprites};
use crate::sim::Sim;
use std::mem::ManuallyDrop;
use windows::core::{Interface, Result};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

const SWAP_FLAGS: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;

pub struct Renderer {
    swap: IDXGISwapChain2,
    dc: ID2D1DeviceContext3,
    batch: ID2D1SpriteBatch,
    atlas: ID2D1Bitmap1,
    waitable: HANDLE,
    width: u32,
    height: u32,
    sprites: Sprites,
    // Gardés vivants : la composition en dépend.
    _visual: IDCompositionVisual,
    _target: IDCompositionTarget,
    _dcomp: IDCompositionDevice,
    _d3d: ID3D11Device,
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.waitable);
        }
    }
}

fn create_d3d() -> Result<ID3D11Device> {
    let mut last = None;
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut dev = None;
        let r = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                None,
            )
        };
        match (r, dev) {
            (Ok(()), Some(d)) => return Ok(d),
            (Err(e), _) => last = Some(e),
            _ => {}
        }
    }
    Err(last.unwrap_or_else(windows::core::Error::empty))
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let width = width.clamp(1, 16384);
        let height = height.clamp(1, 16384);
        unsafe {
            let d3d = create_d3d()?;
            let dxgi_dev: IDXGIDevice = d3d.cast()?;
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: SWAP_FLAGS.0 as u32,
            };
            let swap: IDXGISwapChain2 = factory.CreateSwapChainForComposition(&dxgi_dev, &desc, None)?.cast()?;
            swap.SetMaximumFrameLatency(1)?;
            let waitable = swap.GetFrameLatencyWaitableObject();

            let d2f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2dev = d2f.CreateDevice(&dxgi_dev)?;
            let dc: ID2D1DeviceContext3 = d2dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?.cast()?;
            dc.SetDpi(96.0, 96.0);
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED); // requis par DrawSpriteBatch

            let px = sprites::atlas_pixels();
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let atlas = dc.CreateBitmap(
                D2D_SIZE_U { width: sprites::ATLAS_W, height: sprites::ATLAS_H },
                Some(px.as_ptr() as *const _),
                sprites::ATLAS_W * 4,
                &props,
            )?;
            let batch = dc.CreateSpriteBatch()?;

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_dev)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let mut r = Renderer {
                swap,
                dc,
                batch,
                atlas,
                waitable,
                width,
                height,
                sprites: Sprites::default(),
                _visual: visual,
                _target: target,
                _dcomp: dcomp,
                _d3d: d3d,
            };
            r.bind_target()?;
            Ok(r)
        }
    }

    fn bind_target(&mut self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: ManuallyDrop::new(None),
            };
            let bmp = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.dc.SetTarget(&bmp);
        }
        Ok(())
    }

    /// Objet à attendre avant de dessiner l'image suivante.
    pub fn waitable(&self) -> HANDLE {
        self.waitable
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let width = width.clamp(1, 16384);
        let height = height.clamp(1, 16384);
        if width == self.width && height == self.height {
            return Ok(());
        }
        unsafe {
            self.dc.SetTarget(None);
            self.swap.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, SWAP_FLAGS)?;
        }
        self.width = width;
        self.height = height;
        self.bind_target()
    }

    /// Dessine l'état courant puis présente. Une erreur = périphérique perdu.
    pub fn render(&mut self, sim: &Sim, opacity: f32, sync_interval: u32) -> Result<()> {
        self.sprites.build(sim, self.width as f32, self.height as f32, opacity);
        let sp = &self.sprites;
        unsafe {
            let dc = &self.dc;
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            self.batch.Clear();
            let mut added = Ok(());
            if !sp.rects.is_empty() {
                added = self.batch.AddSprites(
                    sp.rects.len() as u32,
                    sp.rects.as_ptr(),
                    Some(sp.srcs.as_ptr()),
                    Some(sp.colors.as_ptr()),
                    None,
                    std::mem::size_of::<D2D_RECT_F>() as u32,
                    std::mem::size_of::<D2D_RECT_U>() as u32,
                    std::mem::size_of::<D2D1_COLOR_F>() as u32,
                    0,
                );
            }
            let count = self.batch.GetSpriteCount();
            if added.is_ok() && count > 0 {
                dc.DrawSpriteBatch(
                    &self.batch,
                    0,
                    count,
                    &self.atlas,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    D2D1_SPRITE_OPTIONS_NONE,
                );
            }
            dc.EndDraw(None, None)?;
            added?;
            self.swap.Present(sync_interval.clamp(1, 4), DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}
