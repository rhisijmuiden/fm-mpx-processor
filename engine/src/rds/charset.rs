//! UTF-8 → RDS character set (IEC 62106 Annex E, basic Latin + common accents).

pub fn to_rds_byte(c: char) -> u8 {
    match c {
        ' '..='}' if c != '$' && c != '^' && c != '`' => c as u8,
        '$' => 0xAB,
        '^' => 0x5E,
        '`' => 0x60,
        'á' => 0x80, 'à' => 0x81, 'é' => 0x82, 'è' => 0x83, 'í' => 0x84, 'ì' => 0x85,
        'ó' => 0x86, 'ò' => 0x87, 'ú' => 0x88, 'ù' => 0x89, 'Ñ' => 0x8A, 'Ç' => 0x8B,
        'Ş' => 0x8C, 'ß' => 0x8D, '¡' => 0x8E, 'Ĳ' => 0x8F,
        'â' => 0x90, 'ä' => 0x91, 'ê' => 0x92, 'ë' => 0x93, 'î' => 0x94, 'ï' => 0x95,
        'ô' => 0x96, 'ö' => 0x97, 'û' => 0x98, 'ü' => 0x99, 'ñ' => 0x9A, 'ç' => 0x9B,
        'ş' => 0x9C, 'ğ' => 0x9D, 'ı' => 0x9E, 'ĳ' => 0x9F,
        '£' => 0xAA, '€' => 0xA9,
        'Á' => 0xC0, 'À' => 0xC1, 'É' => 0xC2, 'È' => 0xC3, 'Í' => 0xC4, 'Ì' => 0xC5,
        'Ó' => 0xC6, 'Ò' => 0xC7, 'Ú' => 0xC8, 'Ù' => 0xC9,
        'Â' => 0xD0, 'Ä' => 0xD1, 'Ê' => 0xD2, 'Ë' => 0xD3, 'Î' => 0xD4, 'Ï' => 0xD5,
        'Ô' => 0xD6, 'Ö' => 0xD7, 'Û' => 0xD8, 'Ü' => 0xD9,
        'ã' => 0xE0, 'å' => 0xE1, 'æ' => 0xE2, 'ø' => 0xE5, 'õ' => 0xF0,
        'Ã' => 0xE0, 'Å' => 0xE1, 'Æ' => 0xE2, 'Ø' => 0xE5,
        '‘' | '’' => b'\'',
        '“' | '”' => b'"',
        '–' | '—' => b'-',
        _ => b'?',
    }
}

pub fn to_rds_bytes(s: &str) -> Vec<u8> {
    s.chars().map(to_rds_byte).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn ascii_and_accents() {
        assert_eq!(super::to_rds_bytes("Café"), vec![b'C', b'a', b'f', 0x82]);
    }
}
