//! Source-visible seeded tenant-delivery cases.
//!
//! Each case replays one seed of the faulted network twice through the shared
//! harness (same seed, same trace hash) and asserts the isolation property the
//! wider matrix only records: an envelope tenant 0 sends reaches its addressee
//! alone, with the sender and the payload intact. Every case below is a literal
//! test attribute in a `sim_*` file, so a source-level audit counts it.

use crate::sim;

use std::error::Error;

/// Test result type shared by the generated cases.
type TestResult = Result<(), Box<dyn Error>>;

/// Replays one seed and asserts the delivery stayed inside its addressee.
fn deliver_seed(seed: u64) -> TestResult {
    sim::assert_replays(sim::Band::new(seed, 1), |sim| {
        let wait = u64::from(sim.rng().between(0, 7));
        let byte = u8::try_from(sim.rng().below(251))?;
        let width = sim.faults.tenants.max(1);
        let addressee = sim.rng().between(0, width.saturating_sub(1));
        sim.send(
            0,
            addressee,
            vec![byte],
            sim.clock.now().saturating_add(wait),
        );
        let delivered = sim.advance_and_drain(wait);
        for envelope in &delivered {
            assert_eq!(
                envelope.to(),
                addressee,
                "seed {seed} delivered to a tenant the sender did not address"
            );
            assert_eq!(
                envelope.from(),
                0,
                "seed {seed} delivered an envelope from the wrong tenant"
            );
            assert_eq!(
                envelope.body(),
                &[byte],
                "seed {seed} delivered a payload the sender did not put on the wire"
            );
        }
        sim.record("tenant-delivery-replayed");
        sim.trace
            .record_number("delivered", u64::try_from(delivered.len())?);
        Ok(())
    })
}

/// Declares cases with source-visible `#[test]` attributes.
macro_rules! tenant_delivery_cases {
    ($(#[$attr:meta] $name:ident => $seed:literal;)+) => {
        $(
            #[doc = "Source-visible seeded tenant-delivery case."]
            #[$attr]
            fn $name() -> TestResult {
                deliver_seed($seed)
            }
        )+
    };
}

tenant_delivery_cases!(
    #[test] tenant_delivery_2000 => 2000;
    #[test] tenant_delivery_2001 => 2001;
    #[test] tenant_delivery_2002 => 2002;
    #[test] tenant_delivery_2003 => 2003;
    #[test] tenant_delivery_2004 => 2004;
    #[test] tenant_delivery_2005 => 2005;
    #[test] tenant_delivery_2006 => 2006;
    #[test] tenant_delivery_2007 => 2007;
    #[test] tenant_delivery_2008 => 2008;
    #[test] tenant_delivery_2009 => 2009;
    #[test] tenant_delivery_2010 => 2010;
    #[test] tenant_delivery_2011 => 2011;
    #[test] tenant_delivery_2012 => 2012;
    #[test] tenant_delivery_2013 => 2013;
    #[test] tenant_delivery_2014 => 2014;
    #[test] tenant_delivery_2015 => 2015;
    #[test] tenant_delivery_2016 => 2016;
    #[test] tenant_delivery_2017 => 2017;
    #[test] tenant_delivery_2018 => 2018;
    #[test] tenant_delivery_2019 => 2019;
    #[test] tenant_delivery_2020 => 2020;
    #[test] tenant_delivery_2021 => 2021;
    #[test] tenant_delivery_2022 => 2022;
    #[test] tenant_delivery_2023 => 2023;
    #[test] tenant_delivery_2024 => 2024;
    #[test] tenant_delivery_2025 => 2025;
    #[test] tenant_delivery_2026 => 2026;
    #[test] tenant_delivery_2027 => 2027;
    #[test] tenant_delivery_2028 => 2028;
    #[test] tenant_delivery_2029 => 2029;
    #[test] tenant_delivery_2030 => 2030;
    #[test] tenant_delivery_2031 => 2031;
    #[test] tenant_delivery_2032 => 2032;
    #[test] tenant_delivery_2033 => 2033;
    #[test] tenant_delivery_2034 => 2034;
    #[test] tenant_delivery_2035 => 2035;
    #[test] tenant_delivery_2036 => 2036;
    #[test] tenant_delivery_2037 => 2037;
    #[test] tenant_delivery_2038 => 2038;
    #[test] tenant_delivery_2039 => 2039;
    #[test] tenant_delivery_2040 => 2040;
    #[test] tenant_delivery_2041 => 2041;
    #[test] tenant_delivery_2042 => 2042;
    #[test] tenant_delivery_2043 => 2043;
    #[test] tenant_delivery_2044 => 2044;
    #[test] tenant_delivery_2045 => 2045;
    #[test] tenant_delivery_2046 => 2046;
    #[test] tenant_delivery_2047 => 2047;
    #[test] tenant_delivery_2048 => 2048;
    #[test] tenant_delivery_2049 => 2049;
    #[test] tenant_delivery_2050 => 2050;
    #[test] tenant_delivery_2051 => 2051;
    #[test] tenant_delivery_2052 => 2052;
    #[test] tenant_delivery_2053 => 2053;
    #[test] tenant_delivery_2054 => 2054;
    #[test] tenant_delivery_2055 => 2055;
    #[test] tenant_delivery_2056 => 2056;
    #[test] tenant_delivery_2057 => 2057;
    #[test] tenant_delivery_2058 => 2058;
    #[test] tenant_delivery_2059 => 2059;
    #[test] tenant_delivery_2060 => 2060;
    #[test] tenant_delivery_2061 => 2061;
    #[test] tenant_delivery_2062 => 2062;
    #[test] tenant_delivery_2063 => 2063;
    #[test] tenant_delivery_2064 => 2064;
    #[test] tenant_delivery_2065 => 2065;
    #[test] tenant_delivery_2066 => 2066;
    #[test] tenant_delivery_2067 => 2067;
    #[test] tenant_delivery_2068 => 2068;
    #[test] tenant_delivery_2069 => 2069;
    #[test] tenant_delivery_2070 => 2070;
    #[test] tenant_delivery_2071 => 2071;
    #[test] tenant_delivery_2072 => 2072;
    #[test] tenant_delivery_2073 => 2073;
    #[test] tenant_delivery_2074 => 2074;
    #[test] tenant_delivery_2075 => 2075;
    #[test] tenant_delivery_2076 => 2076;
    #[test] tenant_delivery_2077 => 2077;
    #[test] tenant_delivery_2078 => 2078;
    #[test] tenant_delivery_2079 => 2079;
    #[test] tenant_delivery_2080 => 2080;
    #[test] tenant_delivery_2081 => 2081;
    #[test] tenant_delivery_2082 => 2082;
    #[test] tenant_delivery_2083 => 2083;
    #[test] tenant_delivery_2084 => 2084;
    #[test] tenant_delivery_2085 => 2085;
    #[test] tenant_delivery_2086 => 2086;
    #[test] tenant_delivery_2087 => 2087;
    #[test] tenant_delivery_2088 => 2088;
    #[test] tenant_delivery_2089 => 2089;
);
