//! Pure helpers for picking surface configuration.
//!
//! Split out from [`crate::gpu`] so the selection logic is unit-testable
//! without a real GPU adapter or window (`wgpu::Surface`/`Adapter` can only
//! be produced by a live driver).

use crate::error::RendererError;

/// Picks the preferred surface format from a capability list.
///
/// Prefers the first sRGB format (correct color output without manual
/// gamma correction in shaders); falls back to the first format overall.
///
/// # Errors
///
/// Returns [`RendererError::NoSurfaceFormat`] if `formats` is empty.
pub fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> Result<wgpu::TextureFormat, RendererError> {
    formats
        .iter()
        .find(|f| f.is_srgb())
        .or_else(|| formats.first())
        .copied()
        .ok_or(RendererError::NoSurfaceFormat)
}

/// Builds a [`wgpu::SurfaceConfiguration`] for `width`x`height` from the
/// adapter's reported capabilities and a previously chosen format.
///
/// # Errors
///
/// Returns [`RendererError::ZeroSize`] if either dimension is zero (wgpu
/// forbids configuring a surface at zero size).
pub fn build_surface_config(
    capabilities: &wgpu::SurfaceCapabilities,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> Result<wgpu::SurfaceConfiguration, RendererError> {
    if width == 0 || height == 0 {
        return Err(RendererError::ZeroSize { width, height });
    }

    let present_mode = if capabilities
        .present_modes
        .contains(&wgpu::PresentMode::Fifo)
    {
        wgpu::PresentMode::Fifo
    } else {
        capabilities
            .present_modes
            .first()
            .copied()
            .unwrap_or(wgpu::PresentMode::Fifo)
    };

    let alpha_mode = capabilities
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Opaque);

    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width,
        height,
        present_mode,
        desired_maximum_frame_latency: 2,
        alpha_mode,
        view_formats: Vec::new(),
    })
}

/// Decides whether a resize actually needs a surface reconfigure.
///
/// `wgpu::Surface::configure` is not free (it can stall the GPU pipeline
/// on some backends), and window systems routinely fire multiple resize
/// events for the same final size (e.g. two `Resized` events with
/// identical dimensions right after window creation). Skipping a
/// no-op-sized reconfigure avoids that cost.
pub fn should_reconfigure(current: (u32, u32), requested: (u32, u32)) -> bool {
    current != requested
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(present_modes: Vec<wgpu::PresentMode>) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            format_capabilities: Vec::new(),
            present_modes,
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        }
    }

    #[test]
    fn choose_surface_format_prefers_srgb() {
        let formats = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ];
        assert_eq!(
            choose_surface_format(&formats).unwrap(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
    }

    #[test]
    fn choose_surface_format_falls_back_to_first_when_no_srgb() {
        let formats = [wgpu::TextureFormat::Rgba8Unorm];
        assert_eq!(
            choose_surface_format(&formats).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }

    #[test]
    fn choose_surface_format_errors_on_empty() {
        let err = choose_surface_format(&[]).unwrap_err();
        assert!(matches!(err, RendererError::NoSurfaceFormat));
    }

    #[test]
    fn build_surface_config_prefers_fifo() {
        let caps = capabilities(vec![wgpu::PresentMode::Mailbox, wgpu::PresentMode::Fifo]);
        let config =
            build_surface_config(&caps, wgpu::TextureFormat::Bgra8UnormSrgb, 800, 600).unwrap();
        assert_eq!(config.present_mode, wgpu::PresentMode::Fifo);
        assert_eq!(config.width, 800);
        assert_eq!(config.height, 600);
    }

    #[test]
    fn build_surface_config_falls_back_when_fifo_unsupported() {
        let caps = capabilities(vec![wgpu::PresentMode::Immediate]);
        let config =
            build_surface_config(&caps, wgpu::TextureFormat::Bgra8UnormSrgb, 800, 600).unwrap();
        assert_eq!(config.present_mode, wgpu::PresentMode::Immediate);
    }

    #[test]
    fn build_surface_config_rejects_zero_width() {
        let caps = capabilities(vec![wgpu::PresentMode::Fifo]);
        let err =
            build_surface_config(&caps, wgpu::TextureFormat::Bgra8UnormSrgb, 0, 600).unwrap_err();
        assert!(matches!(
            err,
            RendererError::ZeroSize {
                width: 0,
                height: 600
            }
        ));
    }

    #[test]
    fn build_surface_config_rejects_zero_height() {
        let caps = capabilities(vec![wgpu::PresentMode::Fifo]);
        let err =
            build_surface_config(&caps, wgpu::TextureFormat::Bgra8UnormSrgb, 800, 0).unwrap_err();
        assert!(matches!(
            err,
            RendererError::ZeroSize {
                width: 800,
                height: 0
            }
        ));
    }

    #[test]
    fn should_reconfigure_is_false_for_identical_size() {
        assert!(!should_reconfigure((800, 600), (800, 600)));
    }

    #[test]
    fn should_reconfigure_is_true_when_width_changes() {
        assert!(should_reconfigure((800, 600), (801, 600)));
    }

    #[test]
    fn should_reconfigure_is_true_when_height_changes() {
        assert!(should_reconfigure((800, 600), (800, 601)));
    }
}
