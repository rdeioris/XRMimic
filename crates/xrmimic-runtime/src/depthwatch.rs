//! The headset window's check of which way up the app's depth is: every so
//! often it samples both eyes' colour and depth on a small grid, reads the
//! grid back, and lets xrmimic_core::depthcheck vote; enough votes in a row
//! for "upside down" turn the depth over (Shared::depth_flip), which the
//! runtime applies from the next frame.

use crate::xlog;
use windows::core::s;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use xrmimic_core::depthcheck::{scores, Decider, Vote};

/// Grid cells per eye, a side.
const G: u32 = 64;
/// Window frames between checks while the orientation is not settled, and
/// once it is (three upright votes in a row).
const EVERY: u32 = 8;
const EVERY_SETTLED: u32 = 90;

const SHADER: &str = r#"
Texture2D col : register(t0);
Texture2D<float> d0 : register(t1);
Texture2D<float> d1 : register(t2);
SamplerState smp : register(s0);
// Per eye: depth rectangle (x, y, w, h texels) and its range (min, max, near, far).
cbuffer C : register(b0) { float4 r0; float4 r1; float4 i0; float4 i1; float4 grid; };
float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float distance_of(float d, float4 i) {
    float z = saturate((d - i.x) / (i.y - i.x));
    float n = i.z, f = i.w;
    if (isinf(n)) return f / max(z, 1e-6);
    if (isinf(f)) return n / max(1 - z, 1e-6);
    return n * f / (f - z * (f - n));
}
// One texel per cell: x = luminance of the colour, y = log of the distance.
float2 ps(float4 p : SV_Position) : SV_Target {
    uint g = (uint)grid.x;
    uint x = (uint)p.x, y = (uint)p.y;
    uint e = x / g;
    float2 uv = float2(((x % g) + 0.5) / g, (y + 0.5) / g);
    float3 c = col.SampleLevel(smp, float2((e + uv.x) * 0.5, uv.y), 0).rgb;
    float4 r = e == 0 ? r0 : r1;
    int3 pix = int3(r.xy + uv * (r.zw - 1), 0);
    float d = e == 0 ? d0.Load(pix) : d1.Load(pix);
    return float2(dot(c, float3(0.299, 0.587, 0.114)), log(max(distance_of(d, e == 0 ? i0 : i1), 1e-3)));
}
"#;

unsafe fn compile(entry: windows::core::PCSTR, target: windows::core::PCSTR) -> Option<Vec<u8>> {
    let mut blob = None;
    let mut err = None;
    if D3DCompile(SHADER.as_ptr() as *const _, SHADER.len(), None, None, None, entry, target, 0, 0, &mut blob, Some(&mut err))
        .is_err()
    {
        if let Some(err) = err {
            let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
            xlog!("depth check shader: {}", String::from_utf8_lossy(msg));
        }
        return None;
    }
    let blob = blob?;
    Some(std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()).to_vec())
}

/// One eye's depth as the window has it.
pub struct EyeDepth<'a> {
    pub srv: &'a ID3D11ShaderResourceView,
    pub slot: &'a crate::gfx::DepthSlot,
}

pub struct DepthWatch {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    target: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    staging: ID3D11Texture2D,
    decider: Decider,
    upright_in_a_row: u32,
    countdown: u32,
    /// Checks made (the first few are logged with their scores).
    checks: u32,
}

impl DepthWatch {
    pub unsafe fn new(device: &ID3D11Device) -> Option<DepthWatch> {
        let (vs_code, ps_code) = (compile(s!("vs"), s!("vs_5_0"))?, compile(s!("ps"), s!("ps_5_0"))?);
        let (mut vs, mut ps, mut cb, mut target, mut staging, mut rtv) = (None, None, None, None, None, None);
        device.CreateVertexShader(&vs_code, None, Some(&mut vs)).ok()?;
        device.CreatePixelShader(&ps_code, None, Some(&mut ps)).ok()?;
        device
            .CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 5 * 16,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut cb),
            )
            .ok()?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: G * 2,
            Height: G,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R32G32_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        device.CreateTexture2D(&desc, None, Some(&mut target)).ok()?;
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc
        };
        device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).ok()?;
        let target = target?;
        device.CreateRenderTargetView(&target, None, Some(&mut rtv)).ok()?;
        Some(DepthWatch {
            vs: vs?,
            ps: ps?,
            cb: cb?,
            target,
            rtv: rtv?,
            staging: staging?,
            decider: Decider::new(3),
            upright_in_a_row: 0,
            countdown: EVERY,
            checks: 0,
        })
    }

    /// Called once per window frame with the newest frame's colour (both eyes
    /// side by side) and depth. Returns true when the depth should be turned
    /// over. The caller has set nothing it needs afterwards: this pass sets
    /// its own targets, viewport, shaders and resources.
    pub unsafe fn tick(
        &mut self,
        context: &ID3D11DeviceContext,
        sampler: &ID3D11SamplerState,
        colour: &ID3D11ShaderResourceView,
        eyes: [EyeDepth; 2],
    ) -> bool {
        if self.countdown > 0 {
            self.countdown -= 1;
            return false;
        }
        let settled = self.upright_in_a_row >= 3;
        self.countdown = if settled { EVERY_SETTLED } else { EVERY };
        let mut v = Vec::with_capacity(20);
        for e in &eyes {
            let r = e.slot.rect;
            v.extend([r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]);
        }
        for e in &eyes {
            let i = e.slot.info;
            v.extend([i.min, i.max, i.near, i.far]);
        }
        v.extend([G as f32, 0.0, 0.0, 0.0]);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if context.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_err() {
            return false;
        }
        std::ptr::copy_nonoverlapping(v.as_ptr(), mapped.pData as *mut f32, v.len());
        context.Unmap(&self.cb, 0);
        context.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
        context.OMSetBlendState(None, None, 0xffff_ffff);
        context.RSSetState(None);
        context.RSSetViewports(Some(&[D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: (G * 2) as f32,
            Height: G as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }]));
        context.IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context.VSSetShader(&self.vs, None);
        context.PSSetShader(&self.ps, None);
        context.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
        context.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
        context.PSSetShaderResources(0, Some(&[Some(colour.clone()), Some(eyes[0].srv.clone()), Some(eyes[1].srv.clone())]));
        context.Draw(3, 0);
        context.PSSetShaderResources(0, Some(&[None, None, None]));
        context.OMSetRenderTargets(None, None);
        context.CopyResource(&self.staging, &self.target);
        // Reading back stalls until the copy is done: a 128 x 64 grid, a few
        // times a second at most.
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        if context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).is_err() {
            return false;
        }
        let g = G as usize;
        let (mut lum, mut depth) = ([vec![0f32; g * g], vec![0f32; g * g]], [vec![0f32; g * g], vec![0f32; g * g]]);
        for y in 0..g {
            let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize) as *const f32, g * 4);
            for x in 0..g * 2 {
                let (e, cx) = (x / g, x % g);
                lum[e][y * g + cx] = row[x * 2];
                depth[e][y * g + cx] = row[x * 2 + 1];
            }
        }
        context.Unmap(&self.staging, 0);
        // XRMIMIC_DEPTH_CHECK_DUMP=<file>: the first check's grids, for study
        // (per eye: G x G luminance, then G x G log distance, f32 little-endian).
        if self.checks == 0 {
            if let Some(path) = std::env::var_os("XRMIMIC_DEPTH_CHECK_DUMP") {
                let bytes: Vec<u8> = [&lum[0], &depth[0], &lum[1], &depth[1]]
                    .iter()
                    .flat_map(|v| v.iter().flat_map(|f| f.to_le_bytes()))
                    .collect();
                let _ = std::fs::write(path, bytes);
            }
        }
        let s = scores(&[(&lum[0], &depth[0]), (&lum[1], &depth[1])], g);
        let vote = s.vote();
        self.checks += 1;
        if self.checks <= 5 {
            xlog!("depth check {}: scores {:.2} as shown, {:.2} upside down: {vote:?}", self.checks, s.upright, s.flipped);
        }
        if vote == Vote::Upright {
            if self.upright_in_a_row == 2 {
                xlog!("depth check: the depth matches the colour (scores {:.2} as shown, {:.2} upside down)", s.upright, s.flipped);
            }
            self.upright_in_a_row += 1;
        } else if vote == Vote::Flipped {
            self.upright_in_a_row = 0;
        }
        let turn = self.decider.feed(vote);
        if turn {
            self.upright_in_a_row = 0;
            xlog!(
                "depth check: the depth is upside down against the colour (scores {:.2} as shown, {:.2} upside down): turning it over",
                s.upright,
                s.flipped
            );
        }
        turn
    }
}
