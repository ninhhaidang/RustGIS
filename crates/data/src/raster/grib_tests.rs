use super::*;

/// `values` packed in `bits` bits each, most significant first.
fn pack(values: &[u32], bits: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut n) = (0u64, 0u32);
    for &v in values {
        acc = (acc << bits) | u64::from(v);
        n += bits;
        while n >= 8 {
            out.push((acc >> (n - 8)) as u8);
            n -= 8;
        }
    }
    if n > 0 {
        out.push((acc << (8 - n)) as u8);
    }
    out
}

/// A sign bit and magnitude in `n` octets.
fn sm(v: i64, n: usize) -> Vec<u8> {
    let m = v.unsigned_abs() | if v < 0 { 1 << (8 * n - 1) } else { 0 };
    (0..n).rev().map(|i| (m >> (8 * i)) as u8).collect()
}

fn u(v: u64, n: usize) -> Vec<u8> {
    (0..n).rev().map(|i| (v >> (8 * i)) as u8).collect()
}

/// The bits of a bitmap keeping the points `keep` says.
fn bitmap(keep: &[bool]) -> Vec<u8> {
    pack(&keep.iter().map(|&k| u32::from(k)).collect::<Vec<_>>(), 1)
}

struct G1 {
    ni: usize,
    nj: usize,
    /// Millidegrees.
    lat1: i64,
    lon1: i64,
    lat2: i64,
    lon2: i64,
    di: u64,
    dj: u64,
    scan: u8,
    /// IBM reference value, E, D, bits.
    r: u32,
    e: i64,
    d: i64,
    bits: u32,
    /// Packed values, and the bitmap when some points are masked.
    x: Vec<u32>,
    keep: Option<Vec<bool>>,
    /// BDS flag bits (0x40: second-order packing).
    bds_flags: u8,
    param: u8,
}

impl G1 {
    fn new(ni: usize, nj: usize, x: Vec<u32>) -> G1 {
        // 1° cells from 60° N, 10° E, north to south.
        G1 {
            ni,
            nj,
            lat1: 60_000,
            lon1: 10_000,
            lat2: 60_000 - (nj as i64 - 1) * 1000,
            lon2: 10_000 + (ni as i64 - 1) * 1000,
            di: 1000,
            dj: 1000,
            scan: 0,
            r: 0,
            e: 0,
            d: 0,
            bits: 16,
            x,
            keep: None,
            bds_flags: 0,
            param: 11,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut pds = u(28, 3);
        pds.extend([2, 7, 96, 255, 0x80 | if self.keep.is_some() { 0x40 } else { 0 }, self.param, 105]);
        pds.extend(u(2, 2));
        pds.extend([26, 11, 18, 6, 0, 1, 0, 0, 0]);
        pds.extend(u(0, 2));
        pds.extend([0, 21, 0]);
        pds.extend(sm(self.d, 2));
        let mut gds = u(32, 3);
        gds.extend([0, 255, 0]);
        gds.extend(u(self.ni as u64, 2));
        gds.extend(u(self.nj as u64, 2));
        gds.extend(sm(self.lat1, 3));
        gds.extend(sm(self.lon1, 3));
        gds.push(0x80);
        gds.extend(sm(self.lat2, 3));
        gds.extend(sm(self.lon2, 3));
        gds.extend(u(self.di, 2));
        gds.extend(u(self.dj, 2));
        gds.push(self.scan);
        gds.extend([0, 0, 0, 0]);
        let mut body = pds;
        body.extend(gds);
        // Sections of an even number of octets (GRIB1's rule), padded, with their unused bits said.
        if let Some(keep) = &self.keep {
            let mut bits = bitmap(keep);
            if (6 + bits.len()) % 2 == 1 {
                bits.push(0);
            }
            let mut bms = u(6 + bits.len() as u64, 3);
            bms.extend([(bits.len() * 8 - keep.len()) as u8, 0, 0]);
            bms.extend(bits);
            body.extend(bms);
        }
        let mut data = pack(&self.x, self.bits);
        if (11 + data.len()) % 2 == 1 {
            data.push(0);
        }
        let unused = data.len() * 8 - self.x.len() * self.bits as usize;
        let mut bds = u(11 + data.len() as u64, 3);
        bds.push(self.bds_flags | (unused as u8 & 0x0f));
        bds.extend(sm(self.e, 2));
        bds.extend(u(u64::from(self.r), 4));
        bds.push(self.bits as u8);
        bds.extend(data);
        body.extend(bds);
        let mut out = b"GRIB".to_vec();
        out.extend(u(8 + body.len() as u64 + 4, 3));
        out.push(1);
        out.extend(body);
        out.extend(b"7777");
        out
    }
}

/// A GRIB2 field: its grid, product and data sections.
struct F2 {
    ni: usize,
    nj: usize,
    /// Micro-degrees.
    lat1: i64,
    lon1: i64,
    lat2: i64,
    lon2: i64,
    di: u64,
    dj: u64,
    scan: u8,
    template5: u16,
    r: f32,
    e: i64,
    d: i64,
    bits: u32,
    x: Vec<u32>,
    keep: Option<Vec<bool>>,
    category: u8,
    number: u8,
    /// The first fixed surface's scale factor and scaled value.
    surface_scale: u8,
    surface_value: u64,
}

impl F2 {
    fn new(ni: usize, nj: usize, x: Vec<u32>) -> F2 {
        // 0.5° cells from 10° S, 300° E, south to north.
        F2 {
            ni,
            nj,
            lat1: -10_000_000,
            lon1: 300_000_000,
            lat2: -10_000_000 + (nj as i64 - 1) * 500_000,
            lon2: 300_000_000 + (ni as i64 - 1) * 500_000,
            di: 500_000,
            dj: 500_000,
            scan: 0x40,
            template5: 0,
            r: 0.0,
            e: 0,
            d: 0,
            bits: 12,
            x,
            keep: None,
            category: 0,
            number: 0,
            surface_scale: 0,
            surface_value: 2,
        }
    }

    fn sections(&self) -> Vec<u8> {
        let mut s3 = u(72, 4);
        s3.extend([3, 0]);
        s3.extend(u((self.ni * self.nj) as u64, 4));
        s3.extend([0, 0]);
        s3.extend(u(0, 2));
        s3.push(6);
        s3.extend([0; 15]);
        s3.extend(u(self.ni as u64, 4));
        s3.extend(u(self.nj as u64, 4));
        s3.extend(u(0, 4));
        s3.extend(u(u64::from(u32::MAX), 4));
        s3.extend(sm(self.lat1, 4));
        s3.extend(sm(self.lon1, 4));
        s3.push(0x30);
        s3.extend(sm(self.lat2, 4));
        s3.extend(sm(self.lon2, 4));
        s3.extend(u(self.di, 4));
        s3.extend(u(self.dj, 4));
        s3.push(self.scan);
        assert_eq!(s3.len(), 72);
        let mut s4 = u(34, 4);
        s4.push(4);
        s4.extend(u(0, 2));
        s4.extend(u(0, 2));
        s4.extend([self.category, self.number, 2, 0, 96]);
        s4.extend(u(0, 2));
        s4.extend([0, 1]);
        s4.extend(u(6, 4));
        s4.extend([103, self.surface_scale]);
        s4.extend(u(self.surface_value, 4));
        s4.extend([255, 0]);
        s4.extend(u(0, 4));
        assert_eq!(s4.len(), 34);
        // A constant field (0 bits) has no packed values, but still one a point.
        let all = if self.bits == 0 { self.ni * self.nj } else { self.x.len() };
        let count = self.keep.as_ref().map_or(all, |k| k.iter().filter(|b| **b).count());
        let mut s5 = u(21, 4);
        s5.push(5);
        s5.extend(u(count as u64, 4));
        s5.extend(u(u64::from(self.template5), 2));
        s5.extend(u(u64::from(self.r.to_bits()), 4));
        s5.extend(sm(self.e, 2));
        s5.extend(sm(self.d, 2));
        s5.extend([self.bits as u8, 0]);
        let mut s6 = match &self.keep {
            None => {
                let mut s6 = u(6, 4);
                s6.extend([6, 255]);
                s6
            }
            Some(keep) => {
                let bits = bitmap(keep);
                let mut s6 = u(6 + bits.len() as u64, 4);
                s6.extend([6, 0]);
                s6.extend(bits);
                s6
            }
        };
        let data = pack(&self.x, self.bits);
        let mut s7 = u(5 + data.len() as u64, 4);
        s7.push(7);
        s7.extend(data);
        let mut out = s3;
        out.append(&mut s4);
        out.append(&mut s5);
        out.append(&mut s6);
        out.append(&mut s7);
        out
    }
}

/// A GRIB2 message of `fields` (meteorological products, discipline 0).
fn grib2_message(fields: &[F2]) -> Vec<u8> {
    message_of(&fields.iter().flat_map(F2::sections).collect::<Vec<_>>())
}

/// A GRIB2 message of these sections, after its section 1 (meteorological products, discipline 0).
fn message_of(sections: &[u8]) -> Vec<u8> {
    let mut s1 = u(21, 4);
    s1.extend([1, 0, 7, 0, 0, 2, 1, 1]);
    s1.extend(u(2026, 2));
    s1.extend([11, 18, 0, 0, 0, 0, 1]);
    let mut body = s1;
    body.extend(sections);
    let mut out = b"GRIB".to_vec();
    out.extend([0, 0, 0, 2]);
    out.extend(u(16 + body.len() as u64 + 4, 8));
    out.extend(body);
    out.extend(b"7777");
    out
}

fn approx(a: f32, b: f64) -> bool {
    (f64::from(a) - b).abs() < 1e-4
}

#[test]
fn ibm_reference_values() {
    assert_eq!(ibm(0), 0.0);
    assert_eq!(ibm(0x4264_0000), 100.0);
    assert_eq!(ibm(0xc110_0000), -1.0);
    // BAM's land-sea mask (an actual record): R = -4.5555553...
    assert!((ibm(0xc148_e38e) + 4.555_555_3).abs() < 1e-6);
}

#[test]
fn grib1_simple_packing_north_to_south() {
    let mut g = G1::new(3, 2, vec![0, 1, 2, 30, 40, 50]);
    g.r = 0x4264_0000; // 100
    g.d = 1;
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!((r.width, r.height, r.bands.len()), (3, 2, 1));
    let v = &r.bands[0].data;
    for (got, want) in v.iter().zip([10.0, 10.1, 10.2, 13.0, 14.0, 15.0]) {
        assert!(approx(*got, want), "{v:?}");
    }
    let t = r.transform;
    assert_eq!((t.origin_x, t.origin_y, t.pixel_width, t.pixel_height), (9.5, 60.5, 1.0, -1.0));
    assert_eq!(r.bands[0].name, "d=2026111806:kpds5=11:kpds6=105:kpds7=2:TR=0:P1=0:P2=0:TimeU=1");
    // GRIB1's earth is a sphere: said once.
    assert_eq!(r.lost, vec!["its earth is a sphere of radius 6,367,470 m (GRIB1's); latitudes and longitudes are read as WGS 84's"]);
    // 10.1 and 10.2 change in `f32`; 10, 13, 14 and 15 don't.
    assert_eq!(r.rounded, vec![2]);
}

#[test]
fn grib1_bitmap_masks_points() {
    let mut g = G1::new(2, 2, vec![5, 7]);
    g.keep = Some(vec![true, false, false, true]);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    let v = &r.bands[0].data;
    assert_eq!(v[0], 5.0);
    assert!(v[1].is_nan() && v[2].is_nan());
    assert_eq!(v[3], 7.0);
}

#[test]
fn grib1_constant_field() {
    let mut g = G1::new(2, 1, vec![]);
    g.bits = 0;
    g.r = 0x4264_0000;
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![100.0, 100.0]);
}

#[test]
fn bam_latitudes_against_the_scanning_mode() {
    // CPTEC's BAM: the first point at the south pole's row, scanning north to south, round the
    // globe from 0°. Rows follow the scanning mode; columns run from −180°.
    // (GRIB1 increments are two octets of millidegrees: 60° cells.)
    let mut g = G1::new(6, 2, (1..=12).collect());
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj) = (-30_000, 30_000, 0, 300_000, 60_000, 60_000);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    // The first row is the northern one; the 180° column comes first.
    assert_eq!(r.bands[0].data, vec![4.0, 5.0, 6.0, 1.0, 2.0, 3.0, 10.0, 11.0, 12.0, 7.0, 8.0, 9.0]);
    let t = r.transform;
    assert_eq!((t.origin_x, t.origin_y), (-210.0, 60.0));
    assert!(
        r.lost.contains(&"the grid gives its first row at -30° and its last at 30° but scans north to south: its 2 rows of 60° are laid out as the scanning mode says, centred between the two, from 30° to -30°".to_string()),
        "{:?}",
        r.lost
    );
    // BAM's own header: −90° and 90°, the grid's edges, for 1000 rows of 0.18° whose centres
    // its GrADS descriptor puts at ±89.91°.
    let mut g = G1::new(2, 1000, vec![0; 2000]);
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj, g.bits) = (-90_000, 90_000, 0, 180, 180, 180, 1);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert!((r.transform.origin_y - 90.0).abs() < 1e-9, "{:?}", r.transform);
    assert!(r.lost[0].ends_with("from 89.91° to -89.91°"), "{:?}", r.lost);
}

#[test]
fn grib1_second_order_packing_is_refused_by_name() {
    let mut g = G1::new(2, 1, vec![1, 2]);
    g.bds_flags = 0x40;
    let e = read_bytes(&g.bytes(), "t").unwrap_err().to_string();
    assert!(e.contains("nothing in it can be read here: 1 field left out: GRIB1 second-order (complex) packing, not read yet"), "{e}");
}

#[test]
fn grib2_fields_south_to_north_with_a_bitmap() {
    let a = F2::new(2, 2, vec![1, 2, 3, 4]);
    let mut b = F2::new(2, 2, vec![9, 8]);
    (b.r, b.e, b.d, b.category, b.number) = (273.15, 1, 1, 0, 0);
    b.keep = Some(vec![false, true, true, false]);
    let r = read_bytes(&grib2_message(&[a, b]), "t").unwrap();
    assert_eq!(r.bands.len(), 2);
    // Rows flipped north up: the second row scanned is the first.
    assert_eq!(r.bands[0].data, vec![3.0, 4.0, 1.0, 2.0]);
    let v = &r.bands[1].data;
    assert!(v[1].is_nan() && v[2].is_nan());
    // (R + X × 2^E) / 10^D: the point scanned second (east, south) gets 9, the third (west,
    // north) 8.
    assert!(approx(v[3], (273.15f32 as f64 + 9.0 * 2.0) / 10.0) && approx(v[0], (273.15f32 as f64 + 8.0 * 2.0) / 10.0), "{v:?}");
    // From 300° E: moved west of the antimeridian, half a cell west and north of the corners.
    let t = r.transform;
    assert_eq!((t.origin_x, t.origin_y, t.pixel_width, t.pixel_height), (-60.25, -9.25, 0.5, -0.5));
    assert_eq!(r.bands[0].name, "d=2026111800:discipline=0:category=0:number=0:template=4.0:surface=103:value=2:TimeU=1:forecast=6");
    assert!(r.lost.iter().any(|l| l.contains("6,371,229")), "{:?}", r.lost);
}

#[test]
fn grib2_other_packings_are_left_out_and_said() {
    let a = F2::new(2, 1, vec![1, 2]);
    let mut b = F2::new(2, 1, vec![1, 2]);
    b.template5 = 42;
    let r = read_bytes(&grib2_message(&[a, b]), "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert!(r.lost.contains(&"1 field left out: GRIB2 CCSDS (template 5.42), not read yet".to_string()), "{:?}", r.lost);
    // A file with nothing readable is an error naming why.
    let mut c = F2::new(2, 1, vec![1, 2]);
    c.template5 = 40;
    // The surface's value is scaled: 25 at scale factor 1 is 2.5.
    let mut d = F2::new(2, 1, vec![1, 2]);
    (d.surface_scale, d.surface_value) = (1, 25);
    let r = read_bytes(&grib2_message(&[d]), "t").unwrap();
    assert!(r.bands[0].name.contains(":surface=103:value=2.5:"), "{}", r.bands[0].name);
    let e = read_bytes(&grib2_message(&[c]), "t").unwrap_err().to_string();
    assert!(e.contains("JPEG 2000 (template 5.40)"), "{e}");
}

#[test]
fn fields_on_another_grid_are_left_out_and_said() {
    let mut file = G1::new(2, 2, vec![1, 2, 3, 4]).bytes();
    // Padding between messages, as bulletins have.
    file.extend([0u8; 10]);
    file.extend(G1::new(3, 2, vec![1, 2, 3, 4, 5, 6]).bytes());
    file.extend(G1::new(2, 2, vec![5, 6, 7, 8]).bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 2);
    assert_eq!(r.bands[1].data, vec![5.0, 6.0, 7.0, 8.0]);
    assert!(r.lost.contains(&"1 field left out: on another grid than the first field's".to_string()), "{:?}", r.lost);
}

#[test]
fn a_cut_message_is_said_or_refused() {
    let one = G1::new(2, 2, vec![1, 2, 3, 4]).bytes();
    let mut file = one.clone();
    file.extend(&one[..one.len() - 5]);
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert!(r.lost.contains(&"the file ends inside a message, which is left out".to_string()), "{:?}", r.lost);
    let e = read_bytes(&one[..one.len() - 1], "t").unwrap_err().to_string();
    assert!(e.contains("cut short"), "{e}");
    let e = read_bytes(b"not a grib file at all", "t").unwrap_err().to_string();
    assert!(e.contains("no GRIB message"), "{e}");
}

#[test]
fn hostile_bytes_never_panic() {
    let mut file = G1::new(3, 2, vec![0, 1, 2, 3, 4, 5]).bytes();
    file.extend(grib2_message(&[F2::new(2, 2, vec![1, 2, 3, 4])]));
    // Every prefix, and every octet flipped in turn.
    for n in 0..file.len() {
        let _ = read_bytes(&file[..n], "t");
    }
    for i in 0..file.len() {
        for x in [0x00, 0xff, 0x80, 0x7f] {
            let mut bad = file.clone();
            bad[i] = x;
            let _ = read_bytes(&bad, "t");
        }
    }
}

#[test]
fn grib_names_are_rasters() {
    for n in ["a.grb", "A.GRIB", "b.grb2", "c.grib2"] {
        assert!(is_grib_name(n) && crate::raster::is_raster_name(n), "{n}");
    }
    assert!(!is_grib_name("a.grd"));
    let r = crate::raster::read_bytes("run.grb2", &grib2_message(&[F2::new(2, 1, vec![1, 2])])).unwrap();
    assert_eq!(r.name, "run");
}

#[test]
fn another_edition_is_said_and_the_next_message_read() {
    let mut other = G1::new(2, 1, vec![1, 2]).bytes();
    other[7] = 3;
    let mut file = other;
    file.extend(G1::new(2, 1, vec![3, 4]).bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    // Its body isn't said to be bytes outside the messages.
    assert_eq!(
        r.lost.iter().filter(|l| !l.starts_with("its earth")).collect::<Vec<_>>(),
        vec!["1 message left out: not GRIB 1 or 2 (edition 3)"]
    );
    // Nothing readable: the error names why.
    let mut only = G1::new(2, 1, vec![1, 2]).bytes();
    only[7] = 3;
    let e = read_bytes(&only, "t").unwrap_err().to_string();
    assert!(e.contains("1 message left out: not GRIB 1 or 2 (edition 3)"), "{e}");
}

#[test]
fn a_long_gap_between_messages_is_passed_and_said() {
    let mut file = G1::new(2, 1, vec![1, 2]).bytes();
    // About 70 KiB of something else, with "GRI" split over a chunk's end on the way.
    file.extend(std::iter::repeat_n(b'x', 70 * 1024));
    file.extend(b"GRI");
    file.extend(G1::new(2, 1, vec![3, 4]).bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 2);
    assert_eq!(r.bands[1].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&format!("{} bytes outside its messages aren't GRIB, and are passed over", 70 * 1024 + 3)), "{:?}", r.lost);
    // A "GRIB" split over two chunks is found.
    for pad in 4090..4100 {
        let mut file = vec![0u8; pad];
        file.extend(G1::new(2, 1, vec![1, 2]).bytes());
        assert_eq!(read_bytes(&file, "t").unwrap().bands.len(), 1, "{pad}");
    }
}

#[test]
fn ensemble_members_are_named_apart() {
    let mut s = F2::new(2, 1, vec![1, 2]).sections();
    // Section 4 is at 72 (after section 3): template 4.1, with its ensemble octets.
    let at = 72;
    s[at + 8] = 1;
    let mut s4_tail = vec![3u8, 7, 20];
    // Grow section 4 from 34 to 37 octets.
    s.splice(at + 34..at + 34, s4_tail.drain(..));
    s[at + 3] = 37;
    let mut s1 = u(21, 4);
    s1.extend([1, 0, 7, 0, 0, 2, 1, 1]);
    s1.extend(u(2026, 2));
    s1.extend([11, 18, 0, 0, 0, 0, 1]);
    let mut body = s1;
    body.extend(s);
    let mut msg = b"GRIB".to_vec();
    msg.extend([0, 0, 0, 2]);
    msg.extend(u(16 + body.len() as u64 + 4, 8));
    msg.extend(body);
    msg.extend(b"7777");
    let r = read_bytes(&msg, "t").unwrap();
    assert!(r.bands[0].name.ends_with(":template=4.1:surface=103:value=2:TimeU=1:forecast=6:ensemble=3:member=7"), "{}", r.bands[0].name);
}

#[test]
fn a_damaged_field_is_said_as_damaged() {
    let a = F2::new(2, 1, vec![1, 2]);
    let mut msg = grib2_message(&[F2::new(2, 1, vec![1, 2]), F2::new(2, 1, vec![3, 4])]);
    // The second field's data section claims more values than it holds: its 12-bit values need
    // 3 octets; cut its length by giving section 5 a larger count.
    let first = a.sections().len();
    let s5 = 16 + 21 + first + 72 + 34;
    msg[s5 + 5..s5 + 9].copy_from_slice(&u(2000, 4));
    let r = read_bytes(&msg, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert!(r.lost.contains(&"1 field left out: damaged (a field whose values don't fill its grid)".to_string()), "{:?}", r.lost);
    assert!(!r.lost.iter().any(|l| l.contains("not read yet")), "{:?}", r.lost);
}

#[test]
fn grib2_increments_given_are_used() {
    // Code table 3.3: 0x20 and 0x10 say the increments are given; the last point then doesn't
    // decide the cell size.
    let mut f = F2::new(3, 2, vec![1, 2, 3, 4, 5, 6]);
    (f.lon2, f.lat2) = (f.lon1 + 5_000_000, f.lat1 + 3_000_000);
    let r = read_bytes(&grib2_message(&[f]), "t").unwrap();
    assert_eq!((r.transform.pixel_width, r.transform.pixel_height), (0.5, -0.5));
}

#[test]
fn a_length_past_its_message_hides_nothing() {
    let mut first = G1::new(2, 1, vec![1, 2]).bytes();
    let second = G1::new(2, 1, vec![3, 4]).bytes();
    // The first says it is 8 octets longer: its end falls inside the second.
    let len = first.len() as u64 + 8;
    first[4..7].copy_from_slice(&u(len, 3));
    let mut file = first;
    file.extend(&second);
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&"1 message left out: damaged".to_string()), "{:?}", r.lost);
}

#[test]
fn scale_factors_past_f64_give_the_values_they_make() {
    // D = 400: 1 / 10^400 and 2 / 10^400 are 0 in f32, counted as rounded, never an exact 0.
    let mut g = G1::new(2, 1, vec![1, 2]);
    (g.d, g.bits) = (400, 16);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![0.0, 0.0]);
    assert_eq!(r.rounded.first(), Some(&2));
    // D = −400: 10^400 and 2 × 10^400, past f32's range, said.
    let mut g = G1::new(2, 1, vec![1, 2]);
    (g.d, g.bits) = (-400, 16);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert!(r.bands[0].data.iter().all(|v| v.is_nan()));
    assert!(r.lost.contains(&"2 values past the range of 32-bit floats (±3.4 × 10^38) have no value".to_string()), "{:?}", r.lost);
    // A constant field of 0 with D = 400: exactly 0, nothing said.
    let mut g = G1::new(2, 1, vec![]);
    (g.d, g.bits) = (400, 0);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![0.0, 0.0]);
    assert!(r.rounded.iter().all(|n| *n == 0), "{:?}", r.rounded);
    assert!(!r.lost.iter().any(|l| l.contains("past the range") || l.contains("decimal")), "{:?}", r.lost);
    // E = 1030 and D = 310, each past f64 alone: 2^1030 / 10^310, about 1.15, read.
    let mut g = G1::new(2, 1, vec![1, 0]);
    (g.e, g.d, g.bits) = (1030, 310, 16);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    let want = (2f64.powi(1000) / 1e300) * (2f64.powi(30) / 1e10);
    assert!((f64::from(r.bands[0].data[0]) / want - 1.0).abs() < 1e-6, "{:?} {want}", r.bands[0].data);
    assert_eq!(r.bands[0].data[1], 0.0);
    assert!(!r.lost.iter().any(|l| l.contains("past the range")), "{:?}", r.lost);
    // Values past f64 with E = 1000: past f32's range, said (not the whole field refused).
    let mut g = G1::new(2, 1, vec![1, 2]);
    (g.e, g.bits) = (1000, 32);
    g.x = vec![u32::MAX, 1];
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert!(r.bands[0].data.iter().all(|v| v.is_nan()));
    assert!(r.lost.contains(&"2 values past the range of 32-bit floats (±3.4 × 10^38) have no value".to_string()), "{:?}", r.lost);
}

#[test]
fn the_values_present_are_made_whatever_their_scale_factors() {
    // E = 1000 and D = 280 in 32 bits: the largest value the bits could hold is past f64, but
    // 1 × 2^1000 / 10^280 and 2 × 2^1000 / 10^280 aren't.
    let mut g = G1::new(2, 1, vec![1, 2]);
    (g.e, g.d, g.bits) = (1000, 280, 32);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    let one = 2f64.powi(1000) / 1e280;
    for (v, want) in r.bands[0].data.iter().zip([one, 2.0 * one]) {
        assert!((f64::from(*v) / want - 1.0).abs() < 1e-6, "{v} {want}");
    }
    assert!(!r.lost.iter().any(|l| l.contains("past the range")), "{:?}", r.lost);
    // E = -1100 (past f64's exponents) and D = -300: 2^-1100 × 10^300, about 7.4 × 10^-32, kept.
    let mut g = G1::new(2, 1, vec![1, 0]);
    (g.e, g.d) = (-1100, -300);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    let want = 1e300 * 2f64.powi(-1000) * 2f64.powi(-100);
    assert!((f64::from(r.bands[0].data[0]) / want - 1.0).abs() < 1e-6, "{:?}", r.bands[0].data);
    assert_eq!(r.bands[0].data[1], 0.0);
}

#[test]
fn clean_input_says_nothing() {
    // A GRIB2 field on WGS 84 (shape 5), whole numbers in 12 bits, no bitmap: no notice.
    let msg = grib2_message(&[F2::new(2, 2, vec![1, 2, 3, 4])]);
    let mut msg = msg;
    // Section 3 starts after section 0 (16) and section 1 (21); its shape of the earth is octet 15.
    msg[16 + 21 + 14] = 5;
    let r = read_bytes(&msg, "t").unwrap();
    assert!(r.lost.is_empty(), "{:?}", r.lost);
    assert!(r.rounded.is_empty());
}

#[test]
fn a_grib1_quasi_regular_grid_is_said_as_such() {
    let mut g = G1::new(2, 1, vec![1, 2]).bytes();
    // Ni (GDS octets 7 and 8, after the 8-octet indicator and the 28-octet product section).
    g[8 + 28 + 6..8 + 28 + 8].copy_from_slice(&[0xff, 0xff]);
    let e = read_bytes(&g, "t").unwrap_err().to_string();
    assert!(e.contains("quasi-regular grids"), "{e}");
}

#[test]
fn a_length_past_the_file_hides_nothing() {
    let mut first = G1::new(2, 1, vec![1, 2]).bytes();
    let second = G1::new(2, 1, vec![3, 4]).bytes();
    // The first says it runs far past the end of the file.
    first[4..7].copy_from_slice(&u(0x00ff_ffff, 3));
    let mut file = first;
    file.extend(&second);
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&"1 message left out: damaged".to_string()), "{:?}", r.lost);
    assert!(!r.lost.iter().any(|l| l.contains("ends inside")), "{:?}", r.lost);
}

#[test]
fn a_repeated_first_column_is_left_out_and_the_globe_laid_out_from_180() {
    // 7 columns of 60° from 0° to 360°: the last is the first again. No increments given in
    // the file would give the same 60° (360° / 6 intervals).
    let mut g = G1::new(7, 1, vec![1, 2, 3, 4, 5, 6, 1]);
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj) = (0, 0, 0, 0, 60_000, 60_000);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.width, 6);
    // From −180°: the 180° column first.
    assert_eq!(r.bands[0].data, vec![4.0, 5.0, 6.0, 1.0, 2.0, 3.0]);
    assert_eq!(r.transform.origin_x, -210.0);
    assert!(
        r.lost.contains(&"its last column repeats the first, 360° on, and is left out: 6 columns are read".to_string()),
        "{:?}",
        r.lost
    );
}

#[test]
fn a_repeated_first_column_is_not_counted_as_rounded_or_past_the_range() {
    // 7 columns of 60° from 0° to 360°, the last left out: only the 6 laid out are counted.
    // 0.1 to 0.7 less 0.5 (which f32 holds), each rounded by f32: 6, not 7.
    let mut g = G1::new(7, 1, vec![1, 2, 3, 4, 6, 7, 1]);
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj, g.d) = (0, 0, 0, 0, 60_000, 60_000, 1);
    assert_eq!(read_bytes(&g.bytes(), "t").unwrap().rounded, vec![6]);
    // Past f32's range: 6 values said, not 7.
    let mut g = G1::new(7, 1, vec![1, 2, 3, 4, 5, 6, 1]);
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj, g.d) = (0, 0, 0, 0, 60_000, 60_000, -400);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert!(r.lost.contains(&"6 values past the range of 32-bit floats (±3.4 × 10^38) have no value".to_string()), "{:?}", r.lost);
}

#[test]
fn fine_global_grids_a_step_short_of_360_have_no_repeated_column() {
    // 1440 columns of 0.25° and 3600 of 0.1° from 0°, ending a step short of 360°: all read, the
    // 180° column first, from −180°.
    for (di, ni) in [(250, 1440), (100, 3600)] {
        let mut g = G1::new(ni, 1, (0..ni as u32).collect());
        (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj) = (0, 0, 0, (ni as i64 - 1) * di as i64, di, di);
        let r = read_bytes(&g.bytes(), "t").unwrap();
        assert_eq!(r.width as usize, ni);
        assert!(!r.lost.iter().any(|l| l.contains("repeats the first")), "{:?}", r.lost);
        assert_eq!(r.bands[0].data[0], (ni / 2) as f32);
        assert!((r.transform.origin_x - (-180.0 - di as f64 / 2000.0)).abs() < 1e-9, "{}", r.transform.origin_x);
    }
    // One column more, at 360°: the first again, left out.
    let mut g = G1::new(1441, 1, (0..1441).collect());
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di, g.dj) = (0, 0, 0, 360_000, 250, 250);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.width, 1440);
    assert!(r.lost.iter().any(|l| l.contains("repeats the first")), "{:?}", r.lost);
}

#[test]
fn a_sum_of_0_past_f64s_decimal_scale_is_0() {
    // R = −1, D = −400: X = 1 gives (−1 + 1) × 10^400, exactly 0; X = 0 gives −10^400, past f32.
    let mut g = G1::new(2, 1, vec![1, 0]);
    (g.r, g.d, g.bits) = (0xC110_0000, -400, 16);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data[0], 0.0);
    assert!(r.bands[0].data[1].is_nan());
    assert!(r.lost.contains(&"1 value past the range of 32-bit floats (±3.4 × 10^38) has no value".to_string()), "{:?}", r.lost);
}

#[test]
fn a_packed_0_past_f64s_binary_scale_is_the_reference_value() {
    // R = 1, E = 1100, D = 0: X = 0 is 1; X = 1 is 1 + 2^1100, past f32.
    let mut g = G1::new(2, 1, vec![0, 1]);
    (g.r, g.e, g.d, g.bits) = (0x4110_0000, 1100, 0, 16);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data[0], 1.0);
    assert!(r.bands[0].data[1].is_nan());
    assert!(r.lost.contains(&"1 value past the range of 32-bit floats (±3.4 × 10^38) has no value".to_string()), "{:?}", r.lost);
}

#[test]
fn values_divided_down_past_f64_to_0_are_counted_as_rounded() {
    // R = 0, E = −1000, D = 300: X = 1 is about 9 × 10^−602, 0 in f32 and counted; X = 0 is 0.
    let mut g = G1::new(2, 1, vec![1, 0]);
    (g.e, g.d, g.bits) = (-1000, 300, 16);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![0.0, 0.0]);
    assert_eq!(r.rounded, vec![1]);
    // A constant field of R = 16^−65 (about 5 × 10^−79) with D = 300: both points counted.
    let mut g = G1::new(2, 1, vec![]);
    (g.r, g.d, g.bits) = (0x0010_0000, 300, 0);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![0.0, 0.0]);
    assert_eq!(r.rounded, vec![2]);
}

#[test]
fn header_counts_a_last_constant_field() {
    // A constant field's data section is 5 octets, just before "7777".
    let mut c = F2::new(3, 2, vec![]);
    c.bits = 0;
    let msg = grib2_message(&[F2::new(2, 1, vec![1, 2]), c]);
    let n = header(&mut std::io::Cursor::new(&msg), 0, msg.len() as u64, true).unwrap();
    // The smallest field: a message is left out unread only when not even it fits.
    assert_eq!(n, Header::Points(2));
    let one = G1::new(4, 3, vec![0; 12]).bytes();
    assert_eq!(header(&mut std::io::Cursor::new(&one), 0, one.len() as u64, false).unwrap(), Header::Points(12));
}

#[test]
fn grib1_grids_refused_from_the_headers() {
    let at = |g: &[u8]| header(&mut std::io::Cursor::new(g), 0, g.len() as u64, false).unwrap();
    // A Gaussian grid (data representation type 4), in the decoder's words.
    let mut g = G1::new(2, 1, vec![1, 2]).bytes();
    g[8 + 28 + 5] = 4;
    assert_eq!(at(&g), Header::Refused("GRIB1 grids of data representation type 4 (not latitude/longitude)".to_string()));
    let e = read_bytes(&g, "t").unwrap_err().to_string();
    assert!(e.contains("1 field left out: GRIB1 grids of data representation type 4 (not latitude/longitude), not read yet"), "{e}");
    // No grid description: a predefined grid, by its number (octet 7 of the product section).
    let mut g = G1::new(2, 1, vec![1, 2]).bytes();
    g[8 + 7] &= 0x7f;
    g[8 + 6] = 3;
    assert_eq!(at(&g), Header::Refused("predefined grids (grid 3)".to_string()));
    // A row length missing: quasi-regular.
    let mut g = G1::new(2, 1, vec![1, 2]).bytes();
    g[8 + 28 + 8..8 + 28 + 10].copy_from_slice(&[0xff, 0xff]);
    assert_eq!(at(&g), Header::Refused("quasi-regular grids".to_string()));
}

#[test]
fn a_message_of_too_many_sections_is_left_out() {
    // 1100 empty local-use sections (section 2, five octets each) before a field.
    let mut s1 = u(21, 4);
    s1.extend([1, 0, 7, 0, 0, 2, 1, 1]);
    s1.extend(u(2026, 2));
    s1.extend([11, 18, 0, 0, 0, 0, 1]);
    let mut body = s1;
    for _ in 0..1100 {
        body.extend([0, 0, 0, 5, 2]);
    }
    body.extend(F2::new(2, 1, vec![1, 2]).sections());
    let mut msg = b"GRIB".to_vec();
    msg.extend([0, 0, 0, 2]);
    msg.extend(u(16 + body.len() as u64 + 4, 8));
    msg.extend(body);
    msg.extend(b"7777");
    assert_eq!(header(&mut std::io::Cursor::new(&msg), 0, msg.len() as u64, true).unwrap(), Header::TooManySections);
    let mut file = msg;
    file.extend(grib2_message(&[F2::new(2, 1, vec![3, 4])]));
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert!(r.lost.contains(&"1 message left out: damaged (more than 1024 sections)".to_string()), "{:?}", r.lost);
    // The decoder refuses one too, should it get one.
    let first = grib2(&file, usize::MAX, None, Uncounted::default());
    assert!(matches!(first.first(), Some(Err(Skip::Damaged(m))) if m.contains("more than 1024 sections")));
}

#[test]
fn values_past_f32_are_said_not_rounded() {
    // E = 130: 1 × 2^130 is past f32's range; 0 isn't.
    let mut g = G1::new(2, 1, vec![0, 1]);
    g.e = 130;
    let r = read_bytes(&g.bytes(), "t").unwrap();
    let v = &r.bands[0].data;
    assert_eq!(v[0], 0.0);
    assert!(v[1].is_nan(), "{v:?}");
    assert!(r.rounded.is_empty(), "{:?}", r.rounded);
    assert!(r.lost.contains(&"1 value past the range of 32-bit floats (±3.4 × 10^38) has no value".to_string()), "{:?}", r.lost);
    // A constant field whose reference value is past it (IBM 16^60).
    let mut c = G1::new(2, 1, vec![]);
    (c.bits, c.r) = (0, 0x7c10_0000);
    let r = read_bytes(&c.bytes(), "t").unwrap();
    assert!(r.bands[0].data.iter().all(|v| v.is_nan()));
    assert!(r.lost.contains(&"2 values past the range of 32-bit floats (±3.4 × 10^38) have no value".to_string()), "{:?}", r.lost);
}

#[test]
fn a_constant_past_f64_is_said_and_a_reference_that_isnt_a_number_is_damage() {
    // R about 10^10 (IBM 0x4925_40be) and D = −300: R / 10^D is past f64, so past f32.
    let mut c = G1::new(2, 1, vec![]);
    (c.bits, c.r, c.d) = (0, 0x4925_40be, -300);
    let r = read_bytes(&c.bytes(), "t").unwrap();
    assert!(r.bands[0].data.iter().all(|v| v.is_nan()), "{:?}", r.bands[0].data);
    assert!(r.lost.contains(&"2 values past the range of 32-bit floats (±3.4 × 10^38) have no value".to_string()), "{:?}", r.lost);
    assert!(r.rounded.is_empty());
    // A GRIB2 constant field whose IEEE reference value is NaN.
    let mut f = F2::new(2, 1, vec![]);
    (f.bits, f.r) = (0, f32::NAN);
    let e = read_bytes(&grib2_message(&[f]), "t").unwrap_err().to_string();
    assert!(e.contains("damaged (a reference value that isn't a number)"), "{e}");
    // A constant GRIB2 field that is a number reads as one.
    let mut f = F2::new(2, 1, vec![]);
    (f.bits, f.r) = (0, 7.0);
    assert_eq!(read_bytes(&grib2_message(&[f]), "t").unwrap().bands[0].data, vec![7.0, 7.0]);
}

/// A field's sections with its section `number` (4, 5 or 6) cut out.
fn without_section(field: &F2, number: u8) -> Vec<u8> {
    let s = field.sections();
    let mut at = 0;
    while at + 5 <= s.len() {
        let len = u32::from_be_bytes([s[at], s[at + 1], s[at + 2], s[at + 3]]) as usize;
        if s[at + 4] == number {
            let mut out = s[..at].to_vec();
            out.extend(&s[at + len..]);
            return out;
        }
        at += len;
    }
    panic!("no section {number}");
}

#[test]
fn a_grib2_field_without_its_own_sections_is_damaged() {
    for (number, why) in [(4, "its product definition"), (5, "its packing"), (6, "its bitmap section")] {
        let first = F2::new(2, 1, vec![1, 2]);
        let mut second = F2::new(2, 1, vec![3, 4]);
        second.number = 1;
        let mut s1 = u(21, 4);
        s1.extend([1, 0, 7, 0, 0, 2, 1, 1]);
        s1.extend(u(2026, 2));
        s1.extend([11, 18, 0, 0, 0, 0, 1]);
        let mut body = s1;
        body.extend(first.sections());
        body.extend(without_section(&second, number));
        let mut msg = b"GRIB".to_vec();
        msg.extend([0, 0, 0, 2]);
        msg.extend(u(16 + body.len() as u64 + 4, 8));
        msg.extend(body);
        msg.extend(b"7777");
        let r = read_bytes(&msg, "t").unwrap();
        assert_eq!(r.bands.len(), 1, "{number}");
        assert_eq!(r.bands[0].data, vec![1.0, 2.0]);
        assert!(r.lost.contains(&format!("1 field left out: damaged (a field without {why})")), "{number}: {:?}", r.lost);
    }
}

#[test]
fn a_length_that_hides_the_next_message_is_damage() {
    // GRIB1: the first message's length runs to the end of the second, whose "7777" it ends on.
    let mut first = G1::new(2, 1, vec![1, 2]).bytes();
    let second = G1::new(2, 1, vec![3, 4]).bytes();
    let len = (first.len() + second.len()) as u64;
    first[4..7].copy_from_slice(&u(len, 3));
    let mut file = first;
    file.extend(&second);
    assert_eq!(header(&mut std::io::Cursor::new(&file), 0, len, false).unwrap(), Header::WrongLength);
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&"1 message left out: damaged".to_string()), "{:?}", r.lost);
    // GRIB2 alike.
    let mut first = grib2_message(&[F2::new(2, 1, vec![1, 2])]);
    let second = grib2_message(&[F2::new(2, 1, vec![3, 4])]);
    let len = (first.len() + second.len()) as u64;
    first[8..16].copy_from_slice(&u(len, 8));
    let mut file = first;
    file.extend(&second);
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&"1 message left out: damaged".to_string()), "{:?}", r.lost);
}

#[test]
fn grids_are_compared_where_their_points_are() {
    // BAM's latitudes: two southward grids of two 60° rows from −30°, given last at 30° and at
    // 50°, are laid out from 30° and from 40°: not the same grid.
    let grid = |lat2: i64| {
        let mut g = G1::new(2, 2, vec![1, 2, 3, 4]);
        (g.lat1, g.lat2, g.dj) = (-30_000, lat2, 60_000);
        g
    };
    let mut file = grid(30_000).bytes();
    file.extend(grid(50_000).bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert!(r.lost.contains(&"1 field left out: on another grid than the first field's".to_string()), "{:?}", r.lost);
    // The same points scanned from the other end (south to north) are the same grid: a second
    // band, placed by its own scanning mode.
    let mut south_up = G1::new(2, 2, vec![3, 4, 1, 2]);
    (south_up.lat1, south_up.lat2, south_up.scan) = (59_000, 60_000, 0x40);
    let mut file = G1::new(2, 2, vec![1, 2, 3, 4]).bytes();
    file.extend(south_up.bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 2, "{:?}", r.lost);
    assert_eq!(r.bands[0].data, vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(r.bands[1].data, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn only_a_whole_turn_is_rolled_and_only_a_repeated_column_dropped() {
    // Seven columns of 50° from 0° cover 350°: not a whole turn, so not moved.
    let mut g = G1::new(7, 1, vec![1, 2, 3, 4, 5, 6, 7]);
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di) = (0, 0, 0, 300_000, 50_000);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    assert_eq!(r.transform.origin_x, -25.0);
    // Eight columns of 50° from 0° to 350°: the last isn't the first again.
    let mut g = G1::new(8, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    (g.lat1, g.lat2, g.lon1, g.lon2, g.di) = (0, 0, 0, 350_000, 50_000);
    let r = read_bytes(&g.bytes(), "t").unwrap();
    assert_eq!(r.width, 8);
    assert!(!r.lost.iter().any(|l| l.contains("repeats the first")), "{:?}", r.lost);
}

#[test]
fn increments_left_out_follow_the_scanning_direction() {
    // Three columns scanned west from 12° E to 10° E, without increments: 1° cells, not 179°.
    let mut g = G1::new(3, 2, vec![1, 2, 3, 4, 5, 6]);
    (g.lon1, g.lon2, g.scan) = (12_000, 10_000, 0x80);
    let mut b = g.bytes();
    // The resolution flags (GDS octet 17): no increments given.
    b[8 + 28 + 16] = 0;
    let r = read_bytes(&b, "t").unwrap();
    assert_eq!(r.transform.pixel_width, 1.0);
    assert_eq!(r.transform.origin_x, 9.5);
    assert_eq!(r.transform.pixel_height, -1.0);
    assert_eq!(r.bands[0].data, vec![3.0, 2.0, 1.0, 6.0, 5.0, 4.0]);
}

#[test]
fn staggered_rows_are_refused_by_name() {
    let mut f = F2::new(2, 2, vec![1, 2, 3, 4]);
    f.scan = 0x44;
    let e = read_bytes(&grib2_message(&[f]), "t").unwrap_err().to_string();
    assert!(e.contains("grids whose rows are offset or shortened"), "{e}");
}

#[test]
fn short_grib1_sections_are_damage() {
    // A binary data section of 4 octets: no reference value or bits a value.
    let mut c = G1::new(2, 1, vec![]);
    c.bits = 0;
    let full = c.bytes();
    let bds = 8 + 28 + 32;
    let mut b = full[..bds].to_vec();
    b.extend([0, 0, 4, 0]);
    b.extend(b"7777");
    let len = b.len() as u64;
    b[4..7].copy_from_slice(&u(len, 3));
    let e = read_bytes(&b, "t").unwrap_err().to_string();
    assert!(e.contains("damaged (a damaged binary data section)"), "{e}");
}

#[test]
fn a_layer_between_two_surfaces_is_named_by_both() {
    let mut msg = grib2_message(&[F2::new(2, 1, vec![1, 2])]);
    // Section 4 starts after sections 0, 1 and 3; its second surface's type is octet 29, its
    // value octets 31 to 34.
    let s4 = 16 + 21 + 72;
    msg[s4 + 28] = 105;
    msg[s4 + 33] = 5;
    let r = read_bytes(&msg, "t").unwrap();
    let name = &r.bands[0].name;
    assert!(name.contains(":surface=103:value=2:") && name.ends_with(":surface2=105:value2=5"), "{name}");
}

#[test]
fn a_sidecars_nodata_isnt_counted_as_rounded() {
    // D = 1: 0.1 and 0.2, which f32 both rounds; a sidecar's no-data of 0.1 leaves one counted.
    let mut g = G1::new(2, 1, vec![1, 2]);
    g.d = 1;
    let bytes = g.bytes();
    let plain = read_bytes(&bytes, "t").unwrap();
    assert_eq!(plain.rounded, vec![2]);
    let masked = read_with_nodata(std::io::Cursor::new(&bytes), "t", Some(0.1)).unwrap();
    assert_eq!(masked.rounded, vec![1]);
}

#[test]
fn reasons_past_the_most_said_apart_are_counted_together() {
    let mut list = Vec::new();
    for k in 0..20 {
        tally(&mut list, format!("GRIB2 grid template 3.{k}"));
    }
    tally(&mut list, "GRIB2 grid template 3.0".to_string());
    assert_eq!(list.len(), MAX_REASONS + 1);
    assert_eq!(list[0], ("GRIB2 grid template 3.0".to_string(), 2));
    assert_eq!(list[MAX_REASONS], (OTHER_REASONS.to_string(), 4));
}

#[test]
fn grib_is_known_by_its_first_bytes() {
    assert_eq!(crate::io::sniff_extension(&grib2_message(&[F2::new(2, 1, vec![1, 2])])), Some("grb"));
    assert_eq!(crate::io::sniff_extension(&G1::new(2, 1, vec![1, 2]).bytes()), Some("grb"));
}

#[test]
fn a_field_the_message_ends_without_its_data_is_damaged() {
    let first = F2::new(2, 1, vec![1, 2]);
    let mut second = F2::new(2, 1, vec![3, 4]);
    second.number = 1;
    let mut body = first.sections();
    body.extend(without_section(&second, 7));
    let r = read_bytes(&message_of(&body), "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![1.0, 2.0]);
    assert!(r.lost.contains(&"1 field left out: damaged (a field without its data section)".to_string()), "{:?}", r.lost);
}

#[test]
fn one_byte_outside_the_messages_is_said_as_one() {
    let mut file = G1::new(2, 1, vec![1, 2]).bytes();
    file.push(9);
    file.extend(G1::new(2, 1, vec![3, 4]).bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert!(r.lost.contains(&"1 byte outside its messages isn't GRIB, and is passed over".to_string()), "{:?}", r.lost);
}

#[test]
fn grid_notices_past_the_most_said_are_counted() {
    // BAM's latitudes, each pair centred on 0°: the same rows, given by other values each time.
    let mut file = Vec::new();
    for k in 0..20 {
        let mut g = G1::new(2, 2, vec![1, 2, 3, 4]);
        (g.lat1, g.lat2, g.dj) = (-30_000 - k, 30_000 + k, 60_000);
        file.extend(g.bytes());
    }
    // The last grid again: it says what the one before said.
    let mut g = G1::new(2, 2, vec![1, 2, 3, 4]);
    (g.lat1, g.lat2, g.dj) = (-30_019, 30_019, 60_000);
    file.extend(g.bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 21);
    // GRIB1's earth is said once too: 15 of the latitudes' notices, then the 6 fields with more.
    assert!(r.lost.iter().any(|l| l.starts_with("its earth is")), "{:?}", r.lost);
    assert_eq!(r.lost.iter().filter(|l| l.contains("gives its first row")).count(), MAX_REASONS - 1);
    assert!(r.lost.contains(&"more notices about the grids of 6 fields aren't listed".to_string()), "{:?}", r.lost);
}

#[test]
fn a_grid_one_point_across_without_its_spacing_is_refused_by_name() {
    // The resolution flags (GDS octet 17): no increments given.
    let mut b = G1::new(1, 2, vec![1, 2]).bytes();
    b[8 + 28 + 16] = 0;
    let e = read_bytes(&b, "t").unwrap_err().to_string();
    assert!(e.contains("grids one point across that don't give their spacing"), "{e}");
    // Three columns from 0° back to 0°: the only regular grid that ends where it starts goes
    // round once, its last column on its first, which is said and left out.
    let mut g = G1::new(3, 2, vec![1, 2, 3, 4, 5, 6]);
    (g.lon1, g.lon2) = (0, 0);
    let mut b = g.bytes();
    b[8 + 28 + 16] = 0;
    let r = read_bytes(&b, "t").unwrap();
    assert_eq!((r.width, r.transform.pixel_width), (2, 180.0));
    assert!(r.lost.iter().any(|l| l.contains("its last column repeats the first")), "{:?}", r.lost);
}

#[test]
fn surface_values_are_unsigned_or_missing() {
    let mut f = F2::new(2, 1, vec![1, 2]);
    (f.surface_scale, f.surface_value) = (9, 3_000_000_000);
    let name = read_bytes(&grib2_message(&[f]), "t").unwrap().bands[0].name.clone();
    assert!(name.contains(":surface=103:value=3:"), "{name}");
    let mut f = F2::new(2, 1, vec![1, 2]);
    (f.surface_scale, f.surface_value) = (255, u64::from(u32::MAX));
    let name = read_bytes(&grib2_message(&[f]), "t").unwrap().bands[0].name.clone();
    assert!(name.contains(":surface=103:value=missing:"), "{name}");
}

/// A field's sections with its product definition made template 4.`template`, `tail` its octets
/// from 35 on.
fn with_template(field: &F2, template: u8, tail: &[u8]) -> Vec<u8> {
    let mut s = field.sections();
    // Section 4 is at 72 (after section 3), 34 octets long.
    let at = 72;
    s[at + 8] = template;
    s.splice(at + 34..at + 34, tail.iter().copied());
    s[at + 3] = (34 + tail.len()) as u8;
    s
}

#[test]
fn statistics_probabilities_and_percentiles_are_named_apart() {
    // Template 4.8: accumulations (process 1) over 6 and 12 hours from the same forecast time.
    let period = |hours: u64| {
        let mut tail = u(2026, 2);
        tail.extend([11, 18, 12, 0, 0, 1]);
        tail.extend(u(0, 4));
        tail.extend([1, 2, 1]);
        tail.extend(u(hours, 4));
        tail.extend([255]);
        tail.extend(u(0, 4));
        with_template(&F2::new(2, 1, vec![1, 2]), 8, &tail)
    };
    let mut body = period(6);
    body.extend(period(12));
    let r = read_bytes(&message_of(&body), "t").unwrap();
    assert!(r.bands[0].name.ends_with(":forecast=6:statistic=1:rangeU=1:range=6"), "{}", r.bands[0].name);
    assert!(r.bands[1].name.ends_with(":forecast=6:statistic=1:rangeU=1:range=12"), "{}", r.bands[1].name);
    // Template 4.5: above a lower limit of −5 (type 3), the upper one missing.
    let mut tail = vec![1, 1, 3, 0];
    tail.extend(sm(-5, 4));
    tail.push(255);
    tail.extend(u(u64::from(u32::MAX), 4));
    let r = read_bytes(&message_of(&with_template(&F2::new(2, 1, vec![1, 2]), 5, &tail)), "t").unwrap();
    assert!(r.bands[0].name.ends_with(":probability=3:lower=-5:upper=missing"), "{}", r.bands[0].name);
    // Template 4.6: the 90th percentile.
    let r = read_bytes(&message_of(&with_template(&F2::new(2, 1, vec![1, 2]), 6, &[90])), "t").unwrap();
    assert!(r.bands[0].name.ends_with(":percentile=90"), "{}", r.bands[0].name);
}

#[test]
fn fields_named_alike_are_named_by_their_band_too() {
    let mut file = G1::new(2, 1, vec![1, 2]).bytes();
    file.extend(G1::new(2, 1, vec![3, 4]).bytes());
    let r = read_bytes(&file, "t").unwrap();
    assert!(!r.bands[0].name.contains(":band="), "{}", r.bands[0].name);
    assert_eq!(r.bands[1].name, format!("{}:band=2", r.bands[0].name));
}

#[test]
fn a_field_on_another_grid_takes_none_of_the_room() {
    let a = || F2::new(2, 1, vec![1, 2]);
    // Two points 10° east of A's.
    let mut b = F2::new(2, 1, vec![3, 4]);
    (b.lon1, b.lon2) = (310_000_000, 310_500_000);
    let kept = grib2(&grib2_message(&[a()]), usize::MAX, None, Uncounted::default());
    let grid = kept.first().and_then(|f| f.as_ref().ok()).map(|f| f.grid);
    assert!(grid.is_some());
    // Room for two points: B is told apart before it is decoded, so A still fits.
    let got = grib2(&grib2_message(&[b, a()]), 2, grid, Uncounted::default());
    assert!(matches!(got.first(), Some(Err(Skip::OtherGrid))));
    assert!(matches!(got.get(1), Some(Ok(f)) if f.values == vec![1.0, 2.0]));
}

#[test]
fn a_field_whose_sections_come_again_before_its_data_is_damaged() {
    // Field A's sections 3 to 6, no section 7, then field B whole: A is said, B read.
    let a = || {
        let mut a = F2::new(2, 1, vec![1]);
        a.keep = Some(vec![true, false]);
        a
    };
    let b = F2::new(2, 1, vec![3, 4]);
    let mut body = without_section(&a(), 7);
    body.extend(b.sections());
    let r = read_bytes(&message_of(&body), "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&"1 field left out: damaged (a field without its data section)".to_string()), "{:?}", r.lost);
    // Then field B without its bitmap section: never read with A's bitmap.
    let mut body = without_section(&a(), 7);
    body.extend(without_section(&b, 6));
    let got = grib2(&message_of(&body), usize::MAX, None, Uncounted::default());
    assert_eq!(got.len(), 2);
    assert!(matches!(&got[0], Err(Skip::Damaged(why)) if why == "a field without its data section"));
    assert!(matches!(&got[1], Err(Skip::Damaged(why)) if why == "a field without its bitmap section"));
}

/// The offset in a GRIB2 message of its section `number`.
fn section_at(message: &[u8], number: u8) -> usize {
    let mut at = 16;
    while message[at..at + 4] != *b"7777" {
        if message[at + 4] == number {
            return at;
        }
        at += u32::from_be_bytes([message[at], message[at + 1], message[at + 2], message[at + 3]]) as usize;
    }
    panic!("no section {number}");
}

#[test]
fn sections_that_break_before_a_length_that_hides_the_next_message_are_damage() {
    // GRIB1: the first message's binary data section 2 octets long, its length running to the
    // end of the second message.
    let mut first = G1::new(2, 1, vec![1, 2]).bytes();
    let second = G1::new(2, 1, vec![3, 4]).bytes();
    let len = (first.len() + second.len()) as u64;
    first[4..7].copy_from_slice(&u(len, 3));
    let gds = 8 + be(&first, 8, 3) as usize;
    let bds = gds + be(&first, gds, 3) as usize;
    first[bds..bds + 3].copy_from_slice(&u(2, 3));
    let mut file = first;
    file.extend(&second);
    assert_eq!(header(&mut std::io::Cursor::new(&file), 0, len, false).unwrap(), Header::WrongLength);
    let r = read_bytes(&file, "t").unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    assert!(r.lost.contains(&"1 message left out: damaged".to_string()), "{:?}", r.lost);
    // GRIB2: a data section shorter than its header, then one running past the first message's
    // "7777" into the second.
    for grow in [None, Some(2)] {
        let mut first = grib2_message(&[F2::new(2, 1, vec![1, 2])]);
        let second = grib2_message(&[F2::new(2, 1, vec![3, 4])]);
        let len = (first.len() + second.len()) as u64;
        first[8..16].copy_from_slice(&u(len, 8));
        let at = section_at(&first, 7);
        let size = grow.map_or(2, |n| be(&first, at, 4) + n);
        first[at..at + 4].copy_from_slice(&u(u64::from(size), 4));
        let mut file = first;
        file.extend(&second);
        assert_eq!(header(&mut std::io::Cursor::new(&file), 0, len, true).unwrap(), Header::WrongLength, "{grow:?}");
        let r = read_bytes(&file, "t").unwrap();
        assert_eq!(r.bands.len(), 1);
        assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
        assert!(r.lost.contains(&"1 message left out: damaged".to_string()), "{:?}", r.lost);
    }
}

#[test]
fn fields_of_a_message_too_large_to_read_whole_are_said_so() {
    // Room for 4 values: a message of a 2-point field with a megabyte of local use (section 2) is
    // larger than that room lets be read; the next message is read.
    let mut local = u(5 + (1 << 20), 4);
    local.push(2);
    local.resize(5 + (1 << 20), 0);
    local.extend(F2::new(2, 1, vec![1, 2]).sections());
    let mut file = message_of(&local);
    file.extend(grib2_message(&[F2::new(2, 1, vec![3, 4])]));
    let r = read_limited(std::io::Cursor::new(&file), "t", None, 4).unwrap();
    assert_eq!(r.bands.len(), 1);
    assert_eq!(r.bands[0].data, vec![3.0, 4.0]);
    let said = "1 field left out: their message is larger than the memory left lets be read whole, even where one of its fields would fit; split the file with a GRIB tool (wgrib2 or CDO) and add its parts";
    assert!(r.lost.contains(&said.to_string()), "{:?}", r.lost);
    assert!(!r.lost.iter().any(|l| l.contains("the bands read hold the most")), "{:?}", r.lost);
}

/// A field in complex packing: its groups (reference, width in bits and packed values), and the
/// differencing's first values and least difference.
#[derive(Clone)]
struct Cx {
    groups: Vec<(u32, u32, Vec<u32>)>,
    ref_bits: u32,
    missing: u8,
    /// First values (as many as the order; none: template 5.2) and the least difference.
    first: Vec<i64>,
    least: i64,
    octets: usize,
    r: f32,
    e: i64,
    d: i64,
    keep: Option<Vec<bool>>,
}

impl Cx {
    fn new(groups: &[(u32, u32, &[u32])]) -> Cx {
        let groups = groups.iter().map(|(r, w, x)| (*r, *w, x.to_vec())).collect();
        Cx { groups, ref_bits: 9, missing: 0, first: vec![], least: 0, octets: 2, r: 0.0, e: 0, d: 0, keep: None }
    }

    fn differenced(groups: &[(u32, u32, &[u32])], first: &[i64], least: i64) -> Cx {
        Cx { first: first.to_vec(), least, ..Cx::new(groups) }
    }

    fn count(&self) -> usize {
        self.groups.iter().map(|g| g.2.len()).sum()
    }

    /// Sections 5 and 7: widths from 1 less than the least in 5 bits, lengths from 1 by twos in 6
    /// bits (the last group's apart).
    fn sections(&self) -> (Vec<u8>, Vec<u8>) {
        let order = self.first.len();
        let least_width = self.groups.iter().map(|g| g.1).min().unwrap().saturating_sub(1);
        let last = self.groups.last().unwrap().2.len();
        let mut s5 = u(if order > 0 { 49 } else { 47 }, 4);
        s5.push(5);
        s5.extend(u(self.count() as u64, 4));
        s5.extend(u(if order > 0 { 3 } else { 2 }, 2));
        s5.extend(u(u64::from(self.r.to_bits()), 4));
        s5.extend(sm(self.e, 2));
        s5.extend(sm(self.d, 2));
        s5.extend([self.ref_bits as u8, 0, 1, self.missing]);
        s5.extend(u(u64::from(9999f32.to_bits()), 4));
        s5.extend(u(u64::from(9998f32.to_bits()), 4));
        s5.extend(u(self.groups.len() as u64, 4));
        s5.extend([least_width as u8, 5]);
        s5.extend(u(1, 4));
        s5.push(2);
        s5.extend(u(last as u64, 4));
        s5.push(6);
        if order > 0 {
            s5.extend([order as u8, self.octets as u8]);
        }
        let mut data = Vec::new();
        for v in self.first.iter().chain((order > 0).then_some(&self.least)) {
            data.extend(sm(*v, self.octets));
        }
        let list = |f: &dyn Fn(&(u32, u32, Vec<u32>)) -> u32, bits: u32| pack(&self.groups.iter().map(f).collect::<Vec<_>>(), bits);
        data.extend(list(&|g| g.0, self.ref_bits));
        data.extend(list(&|g| g.1 - least_width, 5));
        data.extend(list(&|g| (g.2.len() as u32 - 1) / 2, 6));
        // The groups' values one after another, whatever their widths.
        let mut bits = Vec::new();
        for (_, w, x) in &self.groups {
            bits.extend(x.iter().flat_map(|v| (0..*w).rev().map(move |b| (v >> b) & 1)));
        }
        data.extend(pack(&bits, 1));
        let mut s7 = u(5 + data.len() as u64, 4);
        s7.push(7);
        s7.extend(data);
        (s5, s7)
    }

    /// A message of this field, one row of its points west to east.
    fn message(&self) -> Vec<u8> {
        let (s5, s7) = self.sections();
        self.message_of(&s5, &s7)
    }

    fn message_of(&self, s5: &[u8], s7: &[u8]) -> Vec<u8> {
        let points = self.keep.as_ref().map_or(self.count(), Vec::len);
        let mut f = F2::new(points, 1, vec![0; points]);
        // 0.05° apart, so thousands fit on a row.
        (f.di, f.lon2) = (50_000, f.lon1 + (points as i64 - 1) * 50_000);
        f.keep = self.keep.clone();
        let plain = grib2_message(&[f]);
        let (five, six, seven) = (section_at(&plain, 5), section_at(&plain, 6), section_at(&plain, 7));
        let s1 = 21;
        message_of(&[&plain[16 + s1..five], s5, &plain[six..seven], s7].concat())
    }

    fn values(&self) -> Vec<f32> {
        let r = read_bytes(&self.message(), "t").unwrap();
        assert!(r.lost.iter().all(|l| l.contains("sphere")), "{:?}", r.lost);
        r.bands[0].data.to_vec()
    }
}

/// The values, `None` for NaN.
fn there(v: &[f32]) -> Vec<Option<f32>> {
    v.iter().map(|v| (!v.is_nan()).then_some(*v)).collect()
}

/// Fails with the reason a message is left out for.
fn refused(message: &[u8], why: &str) {
    let e = read_bytes(message, "t").unwrap_err().to_string();
    assert!(e.contains(why), "{e}");
}

#[test]
fn complex_packing_reads_groups_of_their_own_widths() {
    // Groups of 1, 5 and 3 values, the second of no bits (its reference), the last 9 bits wide.
    let mut c = Cx::new(&[(7, 3, &[5]), (300, 0, &[0; 5]), (2, 9, &[0, 511, 100])]);
    assert_eq!(c.values(), vec![12.0, 300.0, 300.0, 300.0, 300.0, 300.0, 2.0, 513.0, 102.0]);
    // (R + X × 2^E) / 10^D, as simple packing's.
    (c.r, c.e, c.d) = (273.5, -1, 1);
    let v = c.values();
    assert!(approx(v[0], (273.5 + 6.0) / 10.0) && approx(v[7], (273.5 + 256.5) / 10.0), "{v:?}");
    // Values f32 rounds are counted, as simple packing's.
    let r = read_bytes(&c.message(), "t").unwrap();
    assert!(r.rounded[0] > 0);
    // A bitmap masks points, and the values are the others'.
    let mut c = Cx::new(&[(1, 2, &[0, 1, 2])]);
    c.keep = Some(vec![true, false, true, true, false]);
    assert_eq!(there(&c.values()), [Some(1.0), None, Some(2.0), Some(3.0), None]);
}

#[test]
fn complex_packing_marks_missing_values() {
    // Primary: every bit set, in a value or in the reference of a group of no bits.
    let mut c = Cx::new(&[(10, 2, &[0, 3, 2]), (511, 0, &[0; 3]), (510, 0, &[0])]);
    c.missing = 1;
    assert_eq!(there(&c.values()), [Some(10.0), None, Some(12.0), None, None, None, Some(510.0)]);
    // The primary and the secondary (one less). In one bit, every value is one or the other.
    let mut c = Cx::new(&[(10, 2, &[0, 3, 2, 1, 1]), (510, 0, &[0; 3]), (509, 0, &[0]), (4, 1, &[0, 1])]);
    c.missing = 2;
    let want = [Some(10.0), None, None, Some(11.0), Some(11.0), None, None, None, Some(509.0), None, None];
    assert_eq!(there(&c.values()), want);
    // Without missing value management, the same bits are values.
    c.missing = 0;
    assert_eq!(c.values(), vec![10.0, 13.0, 12.0, 11.0, 11.0, 510.0, 510.0, 510.0, 509.0, 4.0, 5.0]);
    // None of it is said as a value past the range, nor counted as rounded.
    c.missing = 1;
    let r = read_bytes(&c.message(), "t").unwrap();
    assert!(r.rounded.is_empty() && r.lost.len() == 1, "{:?}", r.lost);
}

#[test]
fn spatial_differencing_is_undone() {
    // 100, 103, 101, 101, 90: differences 3, −2, 0, −11, the least −11.
    let c = Cx::differenced(&[(0, 4, &[0, 14, 9]), (11, 0, &[0]), (0, 1, &[0])], &[100], -11);
    assert_eq!(c.values(), vec![100.0, 103.0, 101.0, 101.0, 90.0]);
    // Twice: 5, 12, 20, 27, 35 differ by 7, 8, 7, 8, which differ by 1, −1, 1.
    let c = Cx::differenced(&[(0, 2, &[0, 0, 2]), (0, 2, &[0, 2])], &[5, 12], -1);
    assert_eq!(c.values(), vec![5.0, 12.0, 20.0, 27.0, 35.0]);
    // The first values are unsigned (the reference value is the least), as GDAL reads them.
    let c = Cx::differenced(&[(0, 4, &[0, 3])], &[40_000], -1);
    assert_eq!(c.values(), vec![40_000.0, 40_002.0]);
    // Missing values aren't differenced: the first value is the first there.
    let mut c = Cx::differenced(&[(0, 3, &[7, 0, 5, 7, 3]), (511, 0, &[0]), (0, 3, &[1])], &[100], -4);
    c.missing = 1;
    assert_eq!(there(&c.values()), [None, Some(100.0), Some(101.0), None, Some(100.0), None, Some(97.0)]);
    let mut c = Cx::differenced(&[(0, 3, &[0, 7, 0, 5, 7, 3])], &[100, 90], -4);
    c.missing = 1;
    assert_eq!(there(&c.values()), [Some(100.0), None, Some(90.0), Some(81.0), None, Some(71.0)]);
    // Descriptors of no octets are 0.
    let mut c = Cx::differenced(&[(0, 4, &[0, 3, 5])], &[0], 0);
    c.octets = 0;
    assert_eq!(c.values(), vec![0.0, 3.0, 8.0]);
}

#[test]
fn a_complex_field_of_no_groups_is_constant() {
    let c = Cx::new(&[(0, 0, &[0, 0, 0])]);
    let (mut s5, _) = c.sections();
    s5[11..15].copy_from_slice(&4.5f32.to_bits().to_be_bytes());
    // No groups, and nothing else of the template set: NCEP's libraries write one so.
    s5[19..].fill(0);
    let r = read_bytes(&c.message_of(&s5, &[0, 0, 0, 5, 7]), "t").unwrap();
    assert_eq!(r.bands[0].data, vec![4.5, 4.5, 4.5]);
}

#[test]
fn complex_packing_this_reader_doesnt_read_is_said() {
    let c = Cx::differenced(&[(0, 4, &[0, 3, 5])], &[1], 0);
    for (octet, value, why) in [
        (22, 0, "group splitting method 0"),
        (23, 3, "missing value management 3"),
        (48, 3, "spatial differencing of order 3"),
        (48, 0, "spatial differencing of order 0"),
        (49, 5, "descriptors of 5 octets"),
    ] {
        let (mut s5, s7) = c.sections();
        s5[octet - 1] = value;
        refused(&c.message_of(&s5, &s7), &format!("1 field left out: GRIB2 complex packing with {why} (template 5.3), not read yet"));
    }
    // References, widths and lengths past 32 bits, given or added up.
    for octet in [20, 36, 37, 47] {
        let (mut s5, mut s7) = c.sections();
        s5[octet - 1] = 33;
        s7.extend([0; 16]);
        s7[3] += 16;
        refused(&c.message_of(&s5, &s7), "1 field left out: values of 3");
    }
}

#[test]
fn damaged_complex_packing_is_said_as_damaged() {
    let c = Cx::new(&[(7, 3, &[5]), (300, 0, &[0; 5]), (2, 9, &[0, 511, 100])]);
    let damaged = |s5: &[u8], s7: &[u8], why: &str| refused(&c.message_of(s5, s7), &format!("1 field left out: damaged ({why})"));
    let (s5, s7) = c.sections();
    // A template cut short.
    let mut short = s5[..46].to_vec();
    short[3] = 46;
    damaged(&short, &s7, "a damaged data representation section");
    // A group's length changed, the last one's, or more groups than values.
    for (octet, value, why) in [
        (41, 2, "a field whose groups don't hold its values"),
        (46, 4, "a field whose groups don't hold its values"),
        (46, 2, "a field whose groups don't hold its values"),
        (35, 10, "a field of more groups than values"),
    ] {
        let mut bad = s5.clone();
        bad[octet - 1] = value;
        damaged(&bad, &s7, why);
    }
    // Data that end in the values, or in the groups' lists.
    for (len, why) in [(s7.len() - 1, "its values"), (9, "its groups"), (5, "its groups")] {
        let mut bad = s7[..len].to_vec();
        bad[3] = len as u8;
        damaged(&s5, &bad, &format!("a data section shorter than {why}"));
    }
    // A reference value that isn't a number.
    let mut bad = s5.clone();
    bad[11..15].copy_from_slice(&f32::NAN.to_bits().to_be_bytes());
    damaged(&bad, &s7, "a reference value that isn't a number");
    // Differences that add up past what a packed value may be: 2^31 more each step, twice over,
    // passes 2^53 within three thousand values.
    let x = [0u32; 127];
    let many: Vec<(u32, u32, &[u32])> = (0..30).map(|_| (0, 0, &x[..])).collect();
    let mut c = Cx::differenced(&many, &[0, 0], (1 << 31) - 1);
    c.octets = 4;
    refused(&c.message(), "1 field left out: damaged (differences that add up past any packed value)");
}

#[test]
fn hostile_complex_packing_never_panics() {
    let mut c = Cx::differenced(&[(0, 3, &[7, 0, 5, 7, 3]), (511, 0, &[0]), (0, 3, &[1])], &[100, 90], -4);
    c.missing = 2;
    let file = c.message();
    for n in 0..file.len() {
        let _ = read_bytes(&file[..n], "t");
    }
    for i in 0..file.len() {
        for x in [0x00, 0x01, 0x20, 0x7f, 0x80, 0xff] {
            let mut bad = file.clone();
            bad[i] = x;
            let _ = read_bytes(&bad, "t");
        }
    }
}
