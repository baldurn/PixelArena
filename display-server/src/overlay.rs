//! Connection instructions drawn over the simulator window, so someone at
//! the screen sees where to point an AI or WLED: the server's own address.

use std::net::{Ipv4Addr, UdpSocket};

use font8x8::{UnicodeFonts, BASIC_FONTS};

const COLOUR: u32 = 0x8c8c8c;
const SHADOW: u32 = 0x000000;

/// This machine's address on the network: the source address of the route
/// to an outside address. Connecting a UDP socket sends nothing.
pub fn local_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?; // TEST-NET-1, never reached
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// The instructions for a display `width` x `height` pixels
pub fn lines(ip: Option<Ipv4Addr>, port: u16, ddp_port: Option<u16>, width: usize, height: usize) -> Vec<String> {
    let host = ip.map_or_else(|| "<no network>".to_string(), |ip| ip.to_string());
    let mut lines = vec![format!("AI url: http://{host}:{port}/")];
    let Some(ddp_port) = ddp_port else { return lines };
    lines.push(match ddp_port {
        4048 => format!("WLED DDP IP: {host}"),
        p => format!("WLED DDP IP: {host} port {p}"),
    });
    lines.push(format!("WLED length {}, 2D matrix {width} wide x {height} high", width * height));
    // The server stretches a canvas with fewer rows (see ddp.rs)
    if height % 2 == 0 {
        lines.push(format!("  or half: length {}, matrix {width} x {}", width * height / 2, height / 2));
    }
    lines
}

/// Draws `lines` in the bottom-left corner of a w x h 0RGB buffer, scaled to the window
pub fn draw(buffer: &mut [u32], w: usize, h: usize, lines: &[String]) {
    let scale = (h / 300).max(1);
    let (glyph, line_h, margin) = (8 * scale, 11 * scale, 4 * scale);
    let top = h.saturating_sub(margin + line_h * lines.len());
    for (n, line) in lines.iter().enumerate() {
        let y0 = top + n * line_h;
        for (i, c) in line.chars().enumerate() {
            let x0 = margin + i * glyph;
            if x0 + glyph > w {
                break;
            }
            let Some(rows) = BASIC_FONTS.get(c) else { continue };
            // A one-pixel shadow keeps the text readable over lit pipes
            for (offset, colour) in [((scale / 2).max(1), SHADOW), (0, COLOUR)] {
                for (ry, bits) in rows.iter().enumerate() {
                    for rx in 0..8 {
                        if bits >> rx & 1 == 0 {
                            continue;
                        }
                        for dy in 0..scale {
                            let y = y0 + ry * scale + dy + offset;
                            if y >= h {
                                continue;
                            }
                            let x = x0 + rx * scale + offset;
                            let end = (x + scale).min(w);
                            if x < end {
                                buffer[y * w + x..y * w + end].fill(colour);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_lines() {
        let ip = Some(Ipv4Addr::new(172, 17, 3, 52));
        assert_eq!(
            lines(ip, 8080, Some(4048), 60, 144),
            [
                "AI url: http://172.17.3.52:8080/",
                "WLED DDP IP: 172.17.3.52",
                "WLED length 8640, 2D matrix 60 wide x 144 high",
                "  or half: length 4320, matrix 60 x 72",
            ]
        );
        assert_eq!(lines(None, 80, None, 60, 144), ["AI url: http://<no network>:80/"]);
        assert_eq!(lines(ip, 80, Some(5000), 4, 3)[1..], ["WLED DDP IP: 172.17.3.52 port 5000", "WLED length 12, 2D matrix 4 wide x 3 high"]);
    }

    #[test]
    fn draws_inside_buffer() {
        let (w, h) = (200, 300);
        let mut buffer = vec![0u32; w * h];
        draw(&mut buffer, w, h, &["AI url: http://172.17.3.52:8080/".into()]);
        assert!(buffer.iter().any(|&p| p == COLOUR));
        // Bottom-left: nothing drawn in the top half
        assert!(buffer[..w * h / 2].iter().all(|&p| p == 0));
    }
}
