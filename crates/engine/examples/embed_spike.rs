//! Measures the cost of streaming a game's frames to Studio: render
//! offscreen with a headless GPU context, read each frame back, and send it
//! over loopback TCP, the transport Play-in-viewport uses.
//!
//! ```sh
//! cargo run -p engine --example embed_spike --release [-- <width> <height> <frames>]
//! ```
//!
//! Prints two measurements: blocking readback latency (render one frame,
//! wait for its pixels), and pipelined throughput (render continuously,
//! take whatever frame is ready, send it). Writes the last frame to
//! `embed_spike.ppm` so its colours can be compared with a window.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use engine::renderer::{CapturedFrame, GpuContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = engine::core::logging::init_default();
    let mut args = std::env::args().skip(1).map(|arg| arg.parse::<u32>());
    let width = args.next().transpose()?.unwrap_or(1280);
    let height = args.next().transpose()?.unwrap_or(720);
    let frames = args.next().transpose()?.unwrap_or(600);

    let gpu = GpuContext::headless(width, height)?;
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    tracing::info!("headless {width}x{height}, {frames} frames, {frame_bytes} bytes per frame");

    // 1. Latency: draw, then block until that frame's pixels are on the CPU.
    let mut latency = Duration::ZERO;
    for index in 0..60 {
        let started = Instant::now();
        draw(&gpu, index)?;
        if gpu.wait_for_frame().is_none() {
            return Err("a blocking readback returned no frame".into());
        }
        latency += started.elapsed();
    }
    tracing::info!(
        "blocking render + readback: {:.2} ms per frame",
        latency.as_secs_f64() * 1000.0 / 60.0
    );

    // 2. Throughput: render continuously, send every finished frame.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let receiver = std::thread::spawn(move || -> std::io::Result<(u64, u64)> {
        let (mut stream, _) = listener.accept()?;
        let (mut count, mut bytes) = (0u64, 0u64);
        let mut header = [0u8; 4];
        let mut payload = Vec::new();
        while stream.read_exact(&mut header).is_ok() {
            let length = u32::from_le_bytes(header) as usize;
            payload.resize(length, 0);
            stream.read_exact(&mut payload)?;
            count += 1;
            bytes += length as u64;
        }
        Ok((count, bytes))
    });
    let mut stream = TcpStream::connect(address)?;
    stream.set_nodelay(true)?;

    let started = Instant::now();
    let mut sent = 0u64;
    let mut last: Option<CapturedFrame> = None;
    for index in 0..frames {
        draw(&gpu, index)?;
        if let Some(frame) = gpu.take_frame() {
            send(&mut stream, &frame)?;
            sent += 1;
            last = Some(frame);
        }
    }
    if let Some(frame) = gpu.wait_for_frame() {
        send(&mut stream, &frame)?;
        sent += 1;
        last = Some(frame);
    }
    let elapsed = started.elapsed().as_secs_f64();
    drop(stream);
    let (received, bytes) = receiver.join().map_err(|_| "receiver thread panicked")??;

    tracing::info!(
        "pipelined: {:.0} rendered fps, {sent} frames sent ({:.0} fps), {} skipped while busy",
        f64::from(frames) / elapsed,
        sent as f64 / elapsed,
        gpu.skipped_frames()
    );
    tracing::info!(
        "received {received} frames, {:.0} MB/s over loopback",
        bytes as f64 / elapsed / 1.0e6
    );
    tracing::info!(
        "at 60 fps this resolution needs {:.0} MB/s",
        frame_bytes as f64 * 60.0 / 1.0e6
    );

    if let Some(frame) = last {
        write_ppm("embed_spike.ppm", &frame)?;
        let centre = ((frame.height / 2 * frame.width + frame.width / 2) * 4) as usize;
        tracing::info!(
            "wrote embed_spike.ppm; centre pixel RGBA = {:?}",
            frame.rgba.get(centre..centre + 4)
        );
    }
    Ok(())
}

/// One frame: a clear colour that changes with `index`, so successive
/// frames differ.
fn draw(gpu: &GpuContext, index: u32) -> Result<(), Box<dyn std::error::Error>> {
    let shade = f64::from(index % 60) / 60.0;
    gpu.render_with(|_, _, encoder, view| {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("embed spike clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.2,
                        g: shade,
                        b: 0.8,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    })?;
    Ok(())
}

fn send(stream: &mut TcpStream, frame: &CapturedFrame) -> std::io::Result<()> {
    let length = u32::try_from(frame.rgba.len()).map_err(std::io::Error::other)?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&frame.rgba)
}

/// Writes `frame` as a binary PPM (opens in Preview), dropping alpha.
fn write_ppm(path: &str, frame: &CapturedFrame) -> std::io::Result<()> {
    let mut bytes = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    let (pixels, _) = frame.rgba.as_chunks::<4>();
    for [r, g, b, _] in pixels {
        bytes.extend_from_slice(&[*r, *g, *b]);
    }
    std::fs::write(path, bytes)
}
