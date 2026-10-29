pub mod check;
pub mod detect;
pub mod email;
pub mod iban;
pub mod ids;
pub mod item;
pub mod live;
pub mod peppol;
pub mod phone;
pub mod source {
    pub mod abn;
    pub mod dns;
    pub mod eos;
    pub mod gleif;
    pub mod hmrc;
    pub mod uid;
    pub mod vies;
}
pub mod text;
pub mod vat;

pub const PHONE_METADATA: &str = "9.0.42";
pub const IBAN_REGISTRY: &str = "103";
pub const PEPPOL_CODE_LIST: &str = "9.7";

/// The version with its data versions, as `--version` prints it after the name.
pub fn version() -> String {
    format!(
        "{} (phone data {PHONE_METADATA}, IBAN registry {IBAN_REGISTRY}, Peppol code list {PEPPOL_CODE_LIST})",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use rlibphonenumber::{PHONE_NUMBER_UTIL, enums::Region};

    #[test]
    fn phone_metadata_knows_current_ranges() {
        let n = PHONE_NUMBER_UTIL.parse("+44 20 7946 0958", None).unwrap();
        assert!(n.is_valid());
        assert_eq!(n.get_region_code(), Some(Region::GB));
        let n = PHONE_NUMBER_UTIL
            .parse("07911 123456", Some(Region::GB))
            .unwrap();
        assert_eq!(n.get_region_code(), Some(Region::GG));
    }

    #[test]
    fn iban_registry_has_its_newest_countries() {
        for iban in [
            "DE44500105175407324931",
            "HN88CABF00000000000250005469",
            "PK36SCBL0000001123456702",
        ] {
            assert_eq!(
                iban_validation_rs::validate_iban_str(iban),
                Ok(true),
                "{iban}"
            );
        }
    }
}
