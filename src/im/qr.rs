use qrcode::QrCode;
use qrcode::render::{svg, unicode};

pub(crate) fn qr_svg(content: &str) -> Result<String, String> {
    let code = QrCode::new(content.as_bytes())
        .map_err(|error| format!("could not encode the iLink QR content: {error}"))?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(256, 256)
        .dark_color(svg::Color("#111827"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

pub(crate) fn qr_terminal(content: &str) -> Result<String, String> {
    let code = QrCode::new(content.as_bytes())
        .map_err(|error| format!("could not encode the iLink QR content: {error}"))?;
    Ok(code
        .render::<unicode::Dense1x2>()
        .module_dimensions(1, 1)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_scannable_surfaces_without_echoing_content_as_text() {
        let content = "https://weixin.qq.com/x/private-qr-value";
        let svg = qr_svg(content).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        assert!(!svg.contains(content));
        let terminal = qr_terminal(content).unwrap();
        assert!(terminal.contains(['█', '▀', '▄']));
        assert!(!terminal.contains(content));
    }
}
