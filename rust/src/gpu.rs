//! GPU SHA-256 prefix search (wgpu: Metal, Vulkan or DX12). Used by `ck worker --gpu`.
//! The GPU only reports which numbers match; their hashes are recomputed on the CPU (few of them).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const WG: u64 = 256; // must match @workgroup_size in sha.wgsl
const BATCH: u64 = WG * 65_535; // numbers per dispatch (max workgroups in one dimension)
const CAP: u64 = 1 << 16; // matches reported per batch; more than that and the batch reruns on the CPU

pub struct Gpu {
    pub name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    out: wgpu::Buffer,
    read: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

impl Gpu {
    pub fn new() -> Option<Gpu> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let opts = wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() };
            let adapter = instance.request_adapter(&opts).await.ok()?;
            let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor::default()).await.ok()?;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("sha"),
                source: wgpu::ShaderSource::Wgsl(include_str!("sha.wgsl").into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("sha"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            let buf = |size: u64, usage| device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage, mapped_at_creation: false });
            use wgpu::BufferUsages as U;
            let params = buf(80, U::STORAGE | U::COPY_DST);
            let out = buf(4 + CAP * 4, U::STORAGE | U::COPY_SRC | U::COPY_DST);
            let read = buf(4 + CAP * 4, U::MAP_READ | U::COPY_DST);
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                ],
            });
            Some(Gpu { name: adapter.get_info().name, device, queue, pipeline, params, out, read, bind })
        })
    }

    /// Numbers in [a, b) whose SHA-256 starts with `nibbles`, or None if the range is out of the
    /// GPU path's limits (caller falls back to the CPU). Stops early when `cancelled` turns true.
    pub fn matches(&self, a: u64, b: u64, nibbles: &[u8], cancelled: &AtomicU64, job: u64) -> Option<Vec<u64>> {
        if nibbles.len() > 64 || b > 4_000_000_000_000_000_000 {
            return None;
        }
        let (mut mask, mut want) = ([0u32; 8], [0u32; 8]);
        for (i, &n) in nibbles.iter().enumerate() {
            let shift = 28 - 4 * (i % 8) as u32;
            mask[i / 8] |= 0xf << shift;
            want[i / 8] |= (n as u32) << shift;
        }
        let mut found = vec![];
        let mut start = a;
        while start < b && cancelled.load(Relaxed) != job {
            let count = (b - start).min(BATCH);
            let mut p = vec![(start % 1_000_000_000) as u32, (start / 1_000_000_000) as u32, count as u32, CAP as u32];
            p.extend(mask);
            p.extend(want);
            let bytes: Vec<u8> = p.iter().flat_map(|v| v.to_le_bytes()).collect();
            self.queue.write_buffer(&self.params, 0, &bytes);
            self.queue.write_buffer(&self.out, 0, &[0; 4]);

            let mut enc = self.device.create_command_encoder(&Default::default());
            {
                let mut pass = enc.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind, &[]);
                pass.dispatch_workgroups(count.div_ceil(WG) as u32, 1, 1);
            }
            enc.copy_buffer_to_buffer(&self.out, 0, &self.read, 0, 4 + CAP * 4);
            self.queue.submit([enc.finish()]);

            let slice = self.read.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
            {
                let data = slice.get_mapped_range().ok()?;
                let words: Vec<u32> = data.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
                let n = words[0] as u64;
                if n > CAP {
                    drop(data);
                    self.read.unmap();
                    return None; // too many matches for the buffer (very short prefix): let the CPU do it
                }
                found.extend(words[1..=n as usize].iter().map(|&i| start + i as u64));
            }
            self.read.unmap();
            start += count;
        }
        found.sort_unstable();
        Some(found)
    }
}
