//! STARLIGHT Bluetooth LED kontrolcüsü (telefon uygulaması "STARLIGHT"). Komutlar `BC` ile başlayıp
//! `55` ile biten kısa paketler, FFF3 özelliğine yazılır; cihaz FFF4'ten cevap verir. Paketler
//! matrix-led'in (Python) çözdüğü protokolden.
//!
//! Ucuz kontrolcülerin çoğu gibi aynı anda tek bağlantı kabul eder: telefon uygulaması açıksa
//! winloom bağlanamaz.

use super::{Cmd, Effect, Model, uuid};

pub static MODEL: Model = Model {
    id: "starlight",
    name: "STARLIGHT",
    matches: |n| n.to_ascii_uppercase().contains("STARLIGHT"),
    write: uuid(0x0000fff3_0000_1000_8000_00805f9b34fb),
    notify: Some(uuid(0x0000fff4_0000_1000_8000_00805f9b34fb)),
    hello: &[0xBC, 0x50, 0x01, 0x01, 0x55],
    effects: EFFECTS,
    reverse: true,
    encode,
};

fn encode(cmd: Cmd) -> Option<Vec<u8>> {
    Some(match cmd {
        Cmd::Power(on) => vec![0xBC, 0x01, 0x01, on as u8, 0x55],
        Cmd::Color([r, g, b]) => {
            let (h, s) = hue_sat(r, g, b);
            let [h1, h0] = h.to_be_bytes();
            let [s1, s0] = s.to_be_bytes();
            vec![0xBC, 0x04, 0x06, h1, h0, s1, s0, 0x00, 0x00, r, g, b, 0x55]
        }
        // 3..1000; daha azında ışık söner.
        Cmd::Brightness(v) => {
            let [v1, v0] = ((v.clamp(0.0, 1.0) * 1000.0).round() as u16).clamp(3, 1000).to_be_bytes();
            vec![0xBC, 0x05, 0x06, v1, v0, 0x00, 0x00, 0x00, 0x00, 0x55]
        }
        Cmd::Effect(id) => {
            // Uygulamadaki sıra ile yazılımdaki numara 112'den sonra kayıyor.
            let id = id.clamp(1, 117);
            let fw = match id {
                112 => 113,
                113.. => id + 2,
                _ => id,
            };
            vec![0xBC, 0x06, 0x02, (fw / 255) as u8, (fw % 255) as u8, 0x55]
        }
        Cmd::Speed(v) => vec![0xBC, 0x08, 0x01, (v.clamp(0.0, 1.0) * 100.0).round() as u8, 0x55],
        Cmd::Reverse(r) => vec![0xBC, 0x07, 0x01, r as u8, 0x55],
    })
}

/// Ton (0..359 derece) ve doygunluk (0..1000), Python'un `colorsys.rgb_to_hsv`'si gibi.
fn hue_sat(r: u8, g: u8, b: u8) -> (u16, u16) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max == min {
        return (0, 0);
    }
    let d = max - min;
    let s = d / max;
    let (rc, gc, bc) = ((max - r) / d, (max - g) / d, (max - b) / d);
    let h = if r == max {
        bc - gc
    } else if g == max {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    let h = (h / 6.0).rem_euclid(1.0);
    // Python'un round'u yarımı çifte yuvarlar.
    ((h * 360.0).round_ties_even() as u16 % 360, (s * 1000.0).round_ties_even() as u16)
}

static EFFECTS: &[Effect] = &[
    Effect { id: 1, name: "Soft Rainbow" },
    Effect { id: 2, name: "Symphony" },
    Effect { id: 3, name: "Color Energy" },
    Effect { id: 4, name: "Color Bounce" },
    Effect { id: 5, name: "RGB Bounce" },
    Effect { id: 6, name: "YCM Bounce" },
    Effect { id: 7, name: "Rainbow Flash" },
    Effect { id: 8, name: "RGB Flash" },
    Effect { id: 9, name: "YCM Flash" },
    Effect { id: 10, name: "Rainbow Breathing" },
    Effect { id: 11, name: "Red / Yellow Fade" },
    Effect { id: 12, name: "Red / Purple Fade" },
    Effect { id: 13, name: "Green Fade" },
    Effect { id: 14, name: "Green / Yellow Fade" },
    Effect { id: 15, name: "Blue / Purple Fade" },
    Effect { id: 16, name: "Red Runner" },
    Effect { id: 17, name: "Green Runner" },
    Effect { id: 18, name: "Blue Runner" },
    Effect { id: 19, name: "Yellow Runner" },
    Effect { id: 20, name: "Cyan Runner" },
    Effect { id: 21, name: "Purple Runner" },
    Effect { id: 22, name: "White Runner" },
    Effect { id: 23, name: "Rainbow Chase" },
    Effect { id: 24, name: "RGB Chase" },
    Effect { id: 25, name: "YCM Chase" },
    Effect { id: 26, name: "Color Flicker" },
    Effect { id: 27, name: "RGB Flicker" },
    Effect { id: 28, name: "YCM Flicker" },
    Effect { id: 29, name: "Rainbow Scan" },
    Effect { id: 30, name: "RGB Scan" },
    Effect { id: 31, name: "YCM Scan" },
    Effect { id: 32, name: "Rainbow Center" },
    Effect { id: 33, name: "RGB Center" },
    Effect { id: 34, name: "YCM Center" },
    Effect { id: 35, name: "Rainbow Open / Close" },
    Effect { id: 36, name: "RGB Open / Close" },
    Effect { id: 37, name: "YCM Open / Close" },
    Effect { id: 38, name: "Red Open / Close" },
    Effect { id: 39, name: "Green Open / Close" },
    Effect { id: 40, name: "Blue Open / Close" },
    Effect { id: 41, name: "Yellow Open / Close" },
    Effect { id: 42, name: "Cyan Open / Close" },
    Effect { id: 43, name: "Purple Open / Close" },
    Effect { id: 44, name: "White Open / Close" },
    Effect { id: 45, name: "Rainbow Pulse" },
    Effect { id: 46, name: "BGR Transition" },
    Effect { id: 47, name: "PGY Transition" },
    Effect { id: 48, name: "Red Pulse" },
    Effect { id: 49, name: "Green Pulse" },
    Effect { id: 50, name: "Blue Pulse" },
    Effect { id: 51, name: "Cyan Pulse" },
    Effect { id: 52, name: "Yellow Pulse" },
    Effect { id: 53, name: "Purple Pulse" },
    Effect { id: 54, name: "White Pulse" },
    Effect { id: 55, name: "Rainbow Wave" },
    Effect { id: 56, name: "BGR Wave" },
    Effect { id: 57, name: "PGY Wave" },
    Effect { id: 58, name: "Red / Green Wave" },
    Effect { id: 59, name: "Green / Blue Wave" },
    Effect { id: 60, name: "Yellow / Blue Wave" },
    Effect { id: 61, name: "Yellow / Cyan Wave" },
    Effect { id: 62, name: "Blue / Purple Wave" },
    Effect { id: 63, name: "Black / White Wave" },
    Effect { id: 64, name: "White / Red Wave" },
    Effect { id: 65, name: "White / Green Wave" },
    Effect { id: 66, name: "White / Blue Wave" },
    Effect { id: 67, name: "White / Yellow Wave" },
    Effect { id: 68, name: "White / Green Wave II" },
    Effect { id: 69, name: "White / Purple Wave" },
    Effect { id: 70, name: "Red / White Wave" },
    Effect { id: 71, name: "Green / White Wave" },
    Effect { id: 72, name: "Blue / White Wave" },
    Effect { id: 73, name: "Yellow / White Wave" },
    Effect { id: 74, name: "Blue / White Wave II" },
    Effect { id: 75, name: "Purple / White Wave" },
    Effect { id: 76, name: "Rainbow Tail" },
    Effect { id: 77, name: "Red Tail" },
    Effect { id: 78, name: "Green Tail" },
    Effect { id: 79, name: "Blue Tail" },
    Effect { id: 80, name: "Yellow Tail" },
    Effect { id: 81, name: "Cyan Tail" },
    Effect { id: 82, name: "Purple Tail" },
    Effect { id: 83, name: "White Tail" },
    Effect { id: 84, name: "Red Run" },
    Effect { id: 85, name: "Green Run" },
    Effect { id: 86, name: "Blue Run" },
    Effect { id: 87, name: "Yellow Run" },
    Effect { id: 88, name: "Cyan Run" },
    Effect { id: 89, name: "Purple Run" },
    Effect { id: 90, name: "White Run" },
    Effect { id: 91, name: "Rainbow Run" },
    Effect { id: 92, name: "BGR Run" },
    Effect { id: 93, name: "PCY Run" },
    Effect { id: 94, name: "BPCY Run" },
    Effect { id: 95, name: "BGCY Run" },
    Effect { id: 96, name: "Red Dot / White" },
    Effect { id: 97, name: "Green Dot / Red" },
    Effect { id: 98, name: "Blue Dot / Green" },
    Effect { id: 99, name: "Yellow Dot / Blue" },
    Effect { id: 100, name: "Blue Dots / Yellow" },
    Effect { id: 101, name: "Purple Dots / Blue" },
    Effect { id: 102, name: "White Dots / Purple" },
    Effect { id: 103, name: "White Dots / Red" },
    Effect { id: 104, name: "Rainbow / Red" },
    Effect { id: 105, name: "Rainbow / Green" },
    Effect { id: 106, name: "Rainbow / Blue" },
    Effect { id: 107, name: "Rainbow / Yellow" },
    Effect { id: 108, name: "Rainbow / Green II" },
    Effect { id: 109, name: "Rainbow / Purple" },
    Effect { id: 110, name: "Rainbow / White" },
    Effect { id: 111, name: "Green Dot / Blue" },
    Effect { id: 112, name: "Green Dot / Red II" },
    Effect { id: 113, name: "Red Dot / Blue" },
    Effect { id: 114, name: "Blue Dots / Yellow II" },
    Effect { id: 115, name: "Yellow Dots / Purple" },
    Effect { id: 116, name: "White Dots / Yellow" },
    Effect { id: 117, name: "Yellow Dots / White" },
];

/// Beklenen paketler matrix-led'in Python koduyla üretildi.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_ile_ayni() {
        assert_eq!(encode(Cmd::Color([255, 0, 0])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0x00, 0x03, 0xE8, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x55], "Cmd::Color([255, 0, 0])");
        assert_eq!(encode(Cmd::Color([0, 255, 0])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0x78, 0x03, 0xE8, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x55], "Cmd::Color([0, 255, 0])");
        assert_eq!(encode(Cmd::Color([0, 0, 255])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0xF0, 0x03, 0xE8, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x55], "Cmd::Color([0, 0, 255])");
        assert_eq!(encode(Cmd::Color([255, 255, 255])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x55], "Cmd::Color([255, 255, 255])");
        assert_eq!(encode(Cmd::Color([0, 0, 0])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55], "Cmd::Color([0, 0, 0])");
        assert_eq!(encode(Cmd::Color([255, 128, 0])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0x1E, 0x03, 0xE8, 0x00, 0x00, 0xFF, 0x80, 0x00, 0x55], "Cmd::Color([255, 128, 0])");
        assert_eq!(encode(Cmd::Color([12, 200, 180])).unwrap(), [0xBC, 0x04, 0x06, 0x00, 0xAE, 0x03, 0xAC, 0x00, 0x00, 0x0C, 0xC8, 0xB4, 0x55], "Cmd::Color([12, 200, 180])");
        assert_eq!(encode(Cmd::Color([128, 0, 255])).unwrap(), [0xBC, 0x04, 0x06, 0x01, 0x0E, 0x03, 0xE8, 0x00, 0x00, 0x80, 0x00, 0xFF, 0x55], "Cmd::Color([128, 0, 255])");
        assert_eq!(encode(Cmd::Color([250, 10, 128])).unwrap(), [0xBC, 0x04, 0x06, 0x01, 0x4A, 0x03, 0xC0, 0x00, 0x00, 0xFA, 0x0A, 0x80, 0x55], "Cmd::Color([250, 10, 128])");
        assert_eq!(encode(Cmd::Brightness(0.0)).unwrap(), [0xBC, 0x05, 0x06, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x55], "Cmd::Brightness(0.0)");
        assert_eq!(encode(Cmd::Brightness(0.003)).unwrap(), [0xBC, 0x05, 0x06, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x55], "Cmd::Brightness(0.003)");
        assert_eq!(encode(Cmd::Brightness(0.5)).unwrap(), [0xBC, 0x05, 0x06, 0x01, 0xF4, 0x00, 0x00, 0x00, 0x00, 0x55], "Cmd::Brightness(0.5)");
        assert_eq!(encode(Cmd::Brightness(0.999)).unwrap(), [0xBC, 0x05, 0x06, 0x03, 0xE7, 0x00, 0x00, 0x00, 0x00, 0x55], "Cmd::Brightness(0.999)");
        assert_eq!(encode(Cmd::Brightness(1.0)).unwrap(), [0xBC, 0x05, 0x06, 0x03, 0xE8, 0x00, 0x00, 0x00, 0x00, 0x55], "Cmd::Brightness(1.0)");
        assert_eq!(encode(Cmd::Effect(1)).unwrap(), [0xBC, 0x06, 0x02, 0x00, 0x01, 0x55], "Cmd::Effect(1)");
        assert_eq!(encode(Cmd::Effect(50)).unwrap(), [0xBC, 0x06, 0x02, 0x00, 0x32, 0x55], "Cmd::Effect(50)");
        assert_eq!(encode(Cmd::Effect(111)).unwrap(), [0xBC, 0x06, 0x02, 0x00, 0x6F, 0x55], "Cmd::Effect(111)");
        assert_eq!(encode(Cmd::Effect(112)).unwrap(), [0xBC, 0x06, 0x02, 0x00, 0x71, 0x55], "Cmd::Effect(112)");
        assert_eq!(encode(Cmd::Effect(113)).unwrap(), [0xBC, 0x06, 0x02, 0x00, 0x73, 0x55], "Cmd::Effect(113)");
        assert_eq!(encode(Cmd::Effect(117)).unwrap(), [0xBC, 0x06, 0x02, 0x00, 0x77, 0x55], "Cmd::Effect(117)");
        assert_eq!(encode(Cmd::Speed(0.0)).unwrap(), [0xBC, 0x08, 0x01, 0x00, 0x55], "Cmd::Speed(0.0)");
        assert_eq!(encode(Cmd::Speed(0.37)).unwrap(), [0xBC, 0x08, 0x01, 0x25, 0x55], "Cmd::Speed(0.37)");
        assert_eq!(encode(Cmd::Speed(1.0)).unwrap(), [0xBC, 0x08, 0x01, 0x64, 0x55], "Cmd::Speed(1.0)");
        assert_eq!(encode(Cmd::Reverse(true)).unwrap(), [0xBC, 0x07, 0x01, 0x01, 0x55], "Cmd::Reverse(true)");
        assert_eq!(encode(Cmd::Reverse(false)).unwrap(), [0xBC, 0x07, 0x01, 0x00, 0x55], "Cmd::Reverse(false)");
        assert_eq!(encode(Cmd::Power(true)).unwrap(), [0xBC, 0x01, 0x01, 0x01, 0x55], "Cmd::Power(true)");
        assert_eq!(encode(Cmd::Power(false)).unwrap(), [0xBC, 0x01, 0x01, 0x00, 0x55], "Cmd::Power(false)");
    }
}
