//! Esri's datum names, by the EPSG geographic system that has each datum (opengeos/RustGIS#2258).
//!
//! ESRI WKT, which ArcGIS, QGIS and GDAL write to `.prj` files, names its datum but gives no code
//! and, most often, no `TOWGS84`. [`super::wkt`] looks the name up here and takes the ellipsoid and
//! the shift to WGS 84 from that system's bundled EPSG definition ([`super::crs::epsg_proj4`]), so
//! a `.prj` without a code reads the same as the system given by its EPSG code. The datums `wkt.rs`
//! names itself (WGS 84 and its equivalents, ED50, OSGB36, DHDN, CH1903 …) are read there first and
//! aren't in this table.
//!
//! Generated from PROJ 9.9.0's database (EPSG dataset v13.102, Esri names from ArcGIS Pro 3.6) with
//! pyproj 3.8.0. Each datum name is the one `CRS.from_epsg(code).to_wkt("WKT1_ESRI")` writes for a
//! bundled EPSG system, normalized as `wkt.rs` normalizes names: upper case, each run of other
//! characters as one `_` (none at either end), and no `D_` prefix. A name is kept when all its
//! systems, projected ones included, have the same ellipsoid and the same shift in their bundled
//! definitions, and that shift is one Helmert step: a `+towgs84` of three or seven parameters, or
//! one of PROJ's datums that has one (MGI, TM65, Carthage …). A system with no shift counts as
//! having a null one, but at least one system must state the shift, even as `+towgs84=0,0,0`. The
//! table maps the name to the lowest geographic 2D code that states it. A prime meridian doesn't
//! matter, because the file gives its own. 323 names are kept, 65 of them with a null shift. Left
//! out:
//!
//! - 267 names whose systems have no Helmert step: PROJ has no transformation for them, or
//!   `crs-definitions` doesn't carry it;
//! - 20 names whose systems disagree, mostly where a projected system has a regional shift: Ukraine
//!   2000, SAD69 (Brazil's systems keep the shift of the deprecated EPSG:4291), Pulkovo 1942(58)
//!   and 1942(83), MGI 1901, MGI (Ferro), Pampa del Castillo, Tananarive 1925, IGN72 Grande Terre,
//!   ST87 Ouvea, Guam 1963 (its Yap Islands system has no shift), NAD83(CSRS), POSGAR 2007 and
//!   seven more. A file on one of these keeps its note;
//! - NAD27 and three other names with a grid on some system, four datums whose shift only
//!   projected systems state, so no geographic code can carry it, and the 20 names `wkt.rs`
//!   already reads.
//!
//! Checked by writing every bundled EPSG system as ESRI WKT with PROJ and reading it back: a system
//! whose datum is here lands within 1 cm of the same system read by its EPSG code. MGI / Austria
//! Central and East (EPSG:9272 and 9273) are the exception. Their definitions spell MGI's Helmert
//! step out as `+towgs84`, while EPSG:4312 names PROJ's `+datum=hermannskogel`, whose shift lands
//! up to 70 cm away. Read by the datum's name, these two systems land where EPSG:4312 puts them,
//! which is also where PROJ itself puts EPSG:9272 and 9273.

/// The EPSG geographic system that has the datum of this name (normalized as above), or `None`.
pub(crate) fn geographic(name: &str) -> Option<u16> {
    NAMES.binary_search_by_key(&name, |&(n, _)| n).ok().and_then(|i| NAMES.get(i)).map(|&(_, c)| c)
}

/// (datum name, EPSG code), sorted by name. The comment is the EPSG name of the system.
#[rustfmt::skip]
pub(crate) const NAMES: &[(&str, u16)] = &[
    ("ABIDJAN_1987", 4143), // Abidjan 1987
    ("ACCRA", 4168), // Accra
    ("ADEN_1925", 6881), // Aden 1925
    ("ADINDAN", 4201), // Adindan
    ("AFGOOYE", 4205), // Afgooye
    ("AIN_EL_ABD_1970", 4204), // Ain el Abd
    ("ALBANIAN_1987", 4191), // Albanian 1987
    ("AMERICAN_SAMOA_1962", 4169), // American Samoa 1962
    ("AMMASSALIK_1958", 4196), // Ammassalik 1958
    ("ANNA_1_1965", 4708), // Cocos Islands 1965
    ("ANTIGUA_1943", 4601), // Antigua 1943
    ("ARATU", 4208), // Aratu
    ("ARC_1950", 4209), // Arc 1950
    ("ARC_1960", 4210), // Arc 1960
    ("ASCENSION_ISLAND_1958", 4712), // Ascension Island 1958
    ("ASTRO_1952", 4711), // Marcus Island 1952
    ("AUSTRALIAN_1966", 4202), // AGD66
    ("AUSTRALIAN_1984", 4203), // AGD84
    ("AUSTRALIAN_ANTARCTIC_1998", 4176), // Australian Antarctic
    ("AYABELLE", 4713), // Ayabelle Lighthouse
    ("AZORES_CENTRAL_ISLANDS_1948", 4183), // Azores Central 1948
    ("AZORES_CENTRAL_ISLANDS_1995", 4665), // Azores Central 1995
    ("AZORES_OCCIDENTAL_ISLANDS_1939", 4182), // Azores Occidental 1939
    ("AZORES_ORIENTAL_ISLANDS_1940", 4184), // Azores Oriental 1940
    ("AZORES_ORIENTAL_ISLANDS_1995", 4664), // Azores Oriental 1995
    ("BARBADOS_1938", 4212), // Barbados 1938
    ("BATAVIA", 4211), // Batavia
    ("BATAVIA_JAKARTA", 4813), // Batavia (Jakarta)
    ("BEACON_E_1945", 4709), // Iwo Jima 1945
    ("BEDUARAM", 4213), // Beduaram
    ("BEIJING_1954", 4214), // Beijing 1954
    ("BEKAA_VALLEY_1920", 6882), // Bekaa Valley 1920
    ("BELGE_1972", 4313), // BD72
    ("BELLEVUE_IGN", 4714), // Bellevue
    ("BERMUDA_1957", 4216), // Bermuda 1957
    ("BERMUDA_2000", 4762), // BDA2000
    ("BHUTAN_NATIONAL_GEODETIC_DATUM", 5264), // DRUKREF 03
    ("BIOKO", 6883), // Bioko
    ("BISSAU", 4165), // Bissau
    ("BOGOTA", 4218), // Bogota 1975
    ("BOGOTA_1975_BOGOTA", 4802), // Bogota 1975 (Bogota)
    ("BONAIRE", 10758), // Bonaire
    ("BUKIT_RIMPAH", 4219), // Bukit Rimpah
    ("CADASTRE_1997", 4475), // Cadastre 1997
    ("CALIFORNIA_SPATIAL_REFERENCE_NETWORK_EPOCH_2025_0_ITRF2020", 10952), // CSRN epoch 2025.0 (ITRF2020)
    ("CAMACUPA", 4220), // Camacupa 1948
    ("CAMPO_INCHAUSPE", 4221), // Campo Inchauspe
    ("CAMP_AREA", 4715), // Camp Area Astro
    ("CANTON_1966", 4716), // Phoenix Islands 1966
    ("CAPE", 4222), // Cape
    ("CAPE_CANAVERAL", 4717), // Cape Canaveral
    ("CARTHAGE", 4223), // Carthage
    ("CAYMAN_ISLANDS_GEODETIC_DATUM_2011", 6135), // CIGD11
    ("CH1903_BERN", 4801), // CH1903 (Bern)
    ("CHATHAM_ISLANDS_1979", 4673), // Chatham Islands 1979
    ("CHATHAM_ISLAND_1971", 4672), // Chatham Islands 1971
    ("CHUA", 4224), // Chua
    ("COMBANI_1950", 4632), // Combani 1950
    ("CONAKRY_1905", 4315), // Conakry 1905
    ("CORREGO_ALEGRE", 4225), // Corrego Alegre 1970-72
    ("COSTA_RICA_2005", 5365), // CR05
    ("CROATIAN_TERRESTRIAL_REFERENCE_SYSTEM", 4761), // ETRS89-HRV [HTRS96]
    ("CSG_1967", 4623), // CSG67
    ("CYPRUS_GEODETIC_REFERENCE_SYSTEM_1993", 6311), // CGRS93
    ("DABOLA_1981", 4155), // Dabola 1981
    ("DATUM_73", 4274), // Datum 73
    ("DATUM_GEODESI_NASIONAL_1995", 4755), // DGN95
    ("DEALUL_PISCULUI_1933", 4316), // Dealul Piscului 1930
    ("DEALUL_PISCULUI_1970", 4317), // Dealul Piscului 1970 (deprecated)
    ("DECEPTION_ISLAND", 4736), // Deception Island
    ("DEIR_EZ_ZOR", 4227), // Deir ez Zor
    ("DOMINICA_1945", 4602), // Dominica 1945
    ("DOS_71_4", 4710), // Astro DOS 71
    ("DOUALA_1948", 4192), // Douala 1948
    ("EASTER_ISLAND_1967", 4719), // Easter Island 1967
    ("EGYPT_1907", 4229), // Egypt 1907
    ("EGYPT_GULF_OF_SUEZ_S_650_TL", 4706), // Egypt Gulf of Suez S-650 TL
    ("ESTONIA_1992", 4133), // EST92
    ("ESTONIA_1997", 4180), // ETRS89-EST [EST97]
    ("EUROPEAN_1950_ED77", 4154), // ED50(ED77)
    ("EUROPEAN_1979", 4668), // ED79
    ("EUROPEAN_1987", 4231), // ED87
    ("EUROPEAN_LIBYAN_1979", 4159), // ELD79
    ("FAHUD", 4232), // Fahud
    ("FATU_IVA_1972", 4688), // Fatu Iva 72
    ("FD_1958", 4132), // FD58
    ("FEHMARNBELT_DATUM_2010", 5593), // FEH2010
    ("FIJI_1956", 4721), // Fiji 1956
    ("FIJI_1986", 4720), // Fiji 1986
    ("FORT_DESAIX", 4625), // Martinique 1938
    ("FORT_MARIGOT", 4621), // Fort Marigot
    ("GAMBIA", 6894), // Gambia
    ("GANDAJIKA_1970", 4233), // Gandajika 1970 (deprecated)
    ("GAN_1970", 4684), // Gan 1970
    ("GDBD2009", 5246), // GDBD2009
    ("GEORGIA_GEODETIC_DATUM", 10831), // Georgia Geodetic Datum
    ("GRAND_CAYMAN_1959", 4723), // GCGD59
    ("GRAND_COMOROS", 4646), // Grand Comoros
    ("GREENLAND_1996", 4747), // GR96
    ("GRENADA_1953", 4603), // Grenada 1953
    ("GUNUNG_SEGARA", 4613), // Segara
    ("GUNUNG_SEGARA_JAKARTA", 4820), // Segara (Jakarta)
    ("HANOI_1972", 4147), // Hanoi 1972
    ("HARTEBEESTHOEK_1994", 4148), // Hartebeesthoek94
    ("HELLE_1954", 4660), // Helle 1954
    ("HERAT_NORTH", 4255), // Herat North
    ("HITO_XVIII_1963", 4254), // Hito XVIII 1963
    ("HJORSEY_1955", 4658), // Hjorsey 1955
    ("HONG_KONG_1963_67", 4739), // Hong Kong 1963(67)
    ("HONG_KONG_1980", 4611), // Hong Kong 1980
    ("HONG_KONG_GEODETIC", 8427), // Hong Kong Geodetic CS
    ("HUNGARIAN_1972", 4237), // HD72
    ("HUNGARIAN_DATUM_1909", 3819), // HD1909
    ("HU_TZU_SHAN", 4236), // Hu Tzu Shan 1950
    ("IGN53_MARE", 4641), // IGN53 Mare
    ("IGN56_LIFOU", 4633), // IGN56 Lifou
    ("IGN63_HIVA_OA", 4689), // IGN63 Hiva Oa
    ("IGN72_NUKU_HIVA", 4630), // IGN72 Nuku Hiva
    ("INDIAN_1954", 4239), // Indian 1954
    ("INDIAN_1960", 4131), // Indian 1960
    ("INDIAN_1975", 4240), // Indian 1975
    ("INDONESIAN_1974", 4238), // ID74
    ("INSTITUT_GEOGRAPHIQUE_DU_CONGO_BELGE_1955", 4701), // IGCB 1955
    ("IRAQI_GEOSPATIAL_REFERENCE_SYSTEM", 3889), // IGRS
    ("IRAQ_KUWAIT_BOUNDARY_1992", 4667), // IKBD-92
    ("IRENET95", 4173), // ETRS89-IRE [ETRF2000]
    ("ISLANDS_NETWORK_1993", 4659), // ISN93
    ("ISLANDS_NETWORK_2004", 5324), // ISN2004
    ("ISRAEL", 4141), // Israel 1993
    ("ISTS_061_1968", 4722), // South Georgia 1968
    ("ISTS_073_1969", 4724), // Diego Garcia 1969
    ("JAMAICA_1969", 4242), // JAD69
    ("JAMAICA_2001", 4758), // JAD2001
    ("JGD_2000", 4612), // JGD2000
    ("JOHNSTON_ISLAND_1961", 4725), // Johnston Island 1961
    ("JOUIK_1961", 4679), // Jouik 1961
    ("KALIANPUR_1937", 4144), // Kalianpur 1937
    ("KALIANPUR_1962", 4145), // Kalianpur 1962
    ("KALIANPUR_1975", 4146), // Kalianpur 1975
    ("KANDAWALA", 4244), // Kandawala
    ("KARBALA_1979_POLSERVICE", 4743), // Karbala 1979
    ("KATANGA_1955", 4695), // Katanga 1955
    ("KERGUELEN_ISLAND_1949", 4631), // K0 1949
    ("KERTAU", 4245), // Kertau 1968
    ("KINGDOM_OF_SAUDI_ARABIA_GEODETIC_REFERENCE_FRAME_2017", 9333), // KSA-GRF17
    ("KKJ", 4123), // KKJ
    ("KOREAN_DATUM_1995", 4166), // Korean 1995
    ("KOREA_GEODETIC_DATUM_2002", 4737), // KGD2002
    ("KUSAIE_1951", 4735), // Kusaie 1951
    ("KUWAIT_OIL_COMPANY", 4246), // KOC
    ("KUWAIT_UTILITY", 4319), // KUDAMS
    ("LAO_NATIONAL_DATUM_1997", 4678), // Lao 1997
    ("LATVIA_1992", 4661), // ETRS89-LVA [LKS-92]
    ("LA_CANOA", 4247), // La Canoa
    ("LEIGON", 4250), // Leigon
    ("LE_POUCE_1934", 4699), // Le Pouce 1934
    ("LIBERIA_1964", 4251), // Liberia 1964
    ("LIBYAN_GEODETIC_DATUM_2006", 4754), // LGD2006
    ("LISBON", 4207), // Lisbon
    ("LISBON_1890", 4666), // Lisbon 1890
    ("LISBON_1890_LISBON", 4904), // Lisbon 1890 (Lisbon)
    ("LISBON_1937_LISBON", 4803), // Lisbon (Lisbon)
    ("LITHUANIA_1994", 4669), // ETRS89-LTU [LKS94]
    ("LITTLE_CAYMAN_1961", 4726), // SIGD61
    ("LOCODJO_1965", 4142), // Locodjo 1965
    ("LUXEMBOURG_REFERENCE_FRAME", 4181), // LUREF
    ("LUZON_1911", 4253), // Luzon 1911
    ("MACAO_2008", 8431), // Macao 2008
    ("MAHE_1971", 4256), // Mahe 1971
    ("MAKASSAR", 4257), // Makassar
    ("MAKASSAR_JAKARTA", 4804), // Makassar (Jakarta)
    ("MALONGO_1987", 4259), // Malongo 1987
    ("MANOCA", 4260), // Manoca (deprecated)
    ("MANOCA_1962", 4193), // Manoca 1962
    ("MARCO_GEODESICO_NACIONAL", 5354), // MARGEN
    ("MASSAWA", 4262), // Massawa
    ("MAUPITI_1983", 4692), // Maupiti 83
    ("MERCHICH", 4261), // Merchich
    ("MEXICAN_DATUM_OF_1993", 4483), // Mexico ITRF92
    ("MEXICO_ITRF2008", 6365), // Mexico ITRF2008
    ("MGI", 4312), // MGI
    ("MHAST", 4264), // Mhast (deprecated)
    ("MIDWAY_1961", 4727), // Midway 1961
    ("MINISTERIO_DE_MARINA_NORTE", 9251), // MMN
    ("MINISTERIO_DE_MARINA_SUR", 9253), // MMS
    ("MINNA", 4263), // Minna
    ("MOLDREF99", 4023), // ETRS89-MDA [MOLDREF99]
    ("MOMRA_TERRESTRIAL_REFERENCE_FRAME_2000", 8818), // MTRF-2000
    ("MONTSERRAT_1958", 4604), // Montserrat 1958
    ("MOOREA_1987", 4691), // Moorea 87
    ("MOP78", 4639), // MOP78
    ("MOZNET", 4130), // Moznet
    ("MPORALOKO", 4266), // M'poraloko
    ("NAHRWAN_1934", 4744), // Nahrwan 1934
    ("NAHRWAN_1967", 4270), // Nahrwan 1967
    ("NAKHL_E_GHANEM", 4693), // Nakhl-e Ghanem
    ("NAPARIMA_1955", 4158), // Naparima 1955
    ("NAPARIMA_1972", 4271), // Naparima 1972
    ("NEA74_NOUMEA", 4644), // NEA74 Noumea
    ("NEPAL_NAGARKOT", 6207), // Nepal 1981
    ("NGN", 4318), // NGN
    ("NGO_1948", 4273), // NGO 1948
    ("NGO_1948_OSLO", 4817), // NGO 1948 (Oslo)
    ("NORD_SAHARA_1959", 4307), // Nord Sahara 1959
    ("NOUAKCHOTT_1965", 4680), // Nouakchott 1965
    ("NOUVELLE_TRIANGULATION_FRANCAISE_PARIS", 4807), // NTF (Paris)
    ("NTF", 4275), // NTF
    ("NZGD_2000", 4167), // NZGD2000
    ("OBSERVATARIO", 4129), // Observatario
    ("OCOTEPEQUE_1935", 5451), // Ocotepeque 1935
    ("OLD_HAWAIIAN", 4135), // Old Hawaiian
    ("OMAN_NATIONAL_GEODETIC_DATUM_2014", 7373), // ONGD14
    ("OMAN_NATIONAL_GEODETIC_DATUM_2017", 9294), // ONGD17
    ("OSNI_1952", 4188), // OSNI 1952
    ("PALESTINE_1923", 4281), // Palestine 1923
    ("PAPUA_NEW_GUINEA_GEODETIC_DATUM_1994", 5546), // PNG94
    ("PARAMETROP_ZEMP_1990", 4740), // PZ-90
    ("PDO_1993", 4134), // PSD93
    ("PERU96", 5373), // Peru96
    ("PETRELS_1972", 4636), // Petrels 1972
    ("PHILIPPINE_REFERENCE_SYSTEM_1992", 4683), // PRS92
    ("PICO_DE_LAS_NIEVES", 4728), // PN84
    ("PICO_DE_LAS_NIEVES_1968", 9403), // PN68
    ("PITCAIRN_1967", 4729), // Pitcairn 1967
    ("PITCAIRN_2006", 4763), // Pitcairn 2006
    ("POINTE_GEOLOGIE_PERROUD_1950", 4637), // Perroud 1950
    ("POINTE_NOIRE", 4282), // Pointe Noire
    ("POINT_58", 4620), // Point 58
    ("PORTO_SANTO_1936", 4615), // Porto Santo
    ("PORTO_SANTO_1995", 4663), // Porto Santo 1995
    ("POSGAR", 4172), // POSGAR (deprecated)
    ("POSGAR_1994", 4694), // POSGAR 94
    ("POSGAR_1998", 4190), // POSGAR 98
    ("PROVISIONAL_S_AMERICAN_1956", 4248), // PSAD56
    ("PTRA08", 5013), // PTRA08
    ("PUERTO_RICO", 4139), // Puerto Rico
    ("PULKOVO_1942", 4284), // Pulkovo 1942
    ("PULKOVO_1995", 4200), // Pulkovo 1995
    ("QATAR", 4285), // Qatar 1974
    ("QND_1995", 4614), // QND95
    ("QORNOQ", 4287), // Qornoq (deprecated)
    ("QORNOQ_1927", 4194), // Qoornoq 1927
    ("RASSADIRAN", 4153), // Rassadiran
    ("RED_GEODESICA_DE_CANARIAS_1995", 4081), // REGCAN95
    ("REGVEN", 4189), // REGVEN
    ("RESEAU_GEODESIQUE_DES_ANTILLES_FRANCAISES_2009", 5489), // RGAF09
    ("RESEAU_GEODESIQUE_DE_LA_RDC_2005", 4046), // RGRDC 2005
    ("RESEAU_GEODESIQUE_DE_MAYOTTE_2004", 4470), // RGM04
    ("RESEAU_GEODESIQUE_DE_NOUVELLE_CALEDONIE_1991_93", 4749), // RGNC91-93
    ("RESEAU_GEODESIQUE_DE_ST_PIERRE_ET_MIQUELON_2006", 4463), // RGSPM06
    ("REUNION_1947", 4626), // Reunion 1947
    ("REYKJAVIK_1900", 4657), // Reykjavik 1900
    ("RGFG_1995", 4624), // RGFG95
    ("RGF_1993", 4171), // ETRS89-FRA [RGF93 v1]
    ("RGR_1992", 4627), // RGR92
    ("ROSS_SEA_REGION_GEODETIC_DATUM_2000", 4764), // RSRGD2000
    ("RRAF_1991", 4558), // RRAF 1991
    ("RT_1990", 4124), // RT90
    ("SABA", 10636), // Saba
    ("SAINTE_ANNE", 4622), // Guadeloupe 1948
    ("SAINT_PIERRE_ET_MIQUELON_1950", 4638), // Saint Pierre et Miquelon 1950
    ("SAMBOJA", 4125), // Samboja (deprecated)
    ("SANTO_DOS_1965", 4730), // Santo 1965
    ("SAPPER_HILL_1943", 4292), // Sapper Hill 1943
    ("SCHWARZECK", 4293), // Schwarzeck
    ("SCORESBYSUND_1952", 4195), // Scoresbysund 1952
    ("SEGORA", 4294), // Segora (deprecated)
    ("SELVAGEM_GRANDE_1938", 4616), // Selvagem Grande
    ("SERBIAN_REFERENCE_NETWORK_1998", 4075), // ETRS89-SRB [SREF98]
    ("SERBIAN_SPATIAL_REFERENCE_SYSTEM_2000", 8685), // ETRS89-SRB [STRS00]
    ("SGNP_MARCARIO_SOLIS", 5371), // MACARIO SOLIS
    ("SIERRA_LEONE_1968", 4175), // Sierra Leone 1968
    ("SINT_EUSTATIUS", 10736), // Sint Eustatius
    ("SIRGAS", 4170), // SIRGAS 1995
    ("SIRGAS_CHILE_REALIZATION_1_EPOCH_2002", 5360), // SIRGAS-Chile 2002
    ("SIRGAS_ES2007_8", 5393), // SIRGAS_ES2007.8
    ("SIRGAS_ROU98", 5381), // SIRGAS-ROU98
    ("SISTEM_REFERENSI_GEOSPASIAL_INDONESIA_2013", 9470), // SRGI2013
    ("SLOVENIA_GEODETIC_DATUM_1996", 4765), // ETRS89-SVN [D96]
    ("SOLOMON_1968", 4718), // Solomon 1968
    ("SOUTH_AMERICAN_DATUM_1969_96", 5527), // SAD69(96)
    ("SOUTH_EAST_ISLAND_1943", 6892), // South East Island 1943
    ("SOUTH_YEMEN", 4164), // South Yemen
    ("SRI_LANKA_DATUM_1999", 5233), // SLD99
    ("ST71_BELEP", 4643), // ST71 Belep
    ("ST84_ILE_DES_PINS", 4642), // ST84 Ile des Pins
    ("ST_HELENA_GEODETIC_DATUM_2015", 7886), // SHGD2015
    ("ST_HELENA_TRITAN", 7881), // St. Helena Tritan
    ("ST_KITTS_1955", 4605), // St. Kitts 1955
    ("ST_LUCIA_1955", 4606), // St. Lucia 1955
    ("ST_VINCENT_1945", 4607), // St. Vincent 1945
    ("SWEREF99", 4619), // ETRS89-SWE [SWEREF 99]
    ("SWISS_TRF_1995", 4151), // ETRS89-CHE [CHTRS95]
    ("S_JTSK_05", 5228), // S-JTSK/05
    ("S_JTSK_05_FERRO", 5229), // S-JTSK/05 (Ferro)
    ("S_JTSK_JTSK03", 8351), // S-JTSK [JTSK03]
    ("TAHAA_1954", 4629), // Tahaa 54
    ("TAHITI_1952", 4628), // Tahiti 52
    ("TAHITI_1979", 4690), // Tahiti 79
    ("TAPI_AIKE", 9248), // Tapi Aike
    ("TERN_ISLAND_1961", 4707), // Tern Island 1961
    ("TETE", 4127), // Tete
    ("TIMBALAI_1948", 4298), // Timbalai 1948
    ("TM65", 4299), // TM65
    ("TM75", 4300), // TM75
    ("TOKYO", 4301), // Tokyo
    ("TRINIDAD_1903", 4302), // Trinidad 1903
    ("TRISTAN_1968", 4734), // Tristan 1968
    ("TURKISH_NATIONAL_REFERENCE_FRAME", 5252), // TUREF
    ("VANUA_LEVU_1915", 4748), // Vanua Levu 1915
    ("VIETNAM_2000", 4756), // VN-2000
    ("VITI_LEVU_1912", 4752), // Viti Levu 1912
    ("VITI_LEVU_1916", 4731), // Viti Levu 1916 (deprecated)
    ("VOIROL_1875", 4304), // Voirol 1875
    ("VOIROL_1875_PARIS", 4811), // Voirol 1875 (Paris)
    ("WAKE_ENIWETOK_1960", 4732), // Marshall Islands 1960
    ("WAKE_ISLAND_1952", 4733), // Wake Island 1952
    ("WGS_1972", 4322), // WGS 72
    ("WGS_1972_BE", 4324), // WGS 72BE
    ("YACARE", 4309), // Yacare
    ("YEMEN_NGN_1996", 4163), // Yemen NGN96
    ("YOFF", 4310), // Yoff
    ("ZANDERIJ", 4311), // Zanderij
];
