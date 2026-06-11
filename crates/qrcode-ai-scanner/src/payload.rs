//! Typed payload classification — the structured layer the scan tool page
//! and Nika workflows consume.
//!
//! Policy: values are split structurally but NOT percent-decoded — decoding
//! presentation is the consumer's choice. Anything unparseable falls back to
//! [`Payload::Text`]; classification never fails and never panics.

/// Classified payload of a decoded QR content string.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Payload {
    /// Web link (`http://` / `https://`).
    Url {
        /// The full original URL string.
        url: String,
    },
    /// Wi-Fi network join card (`WIFI:`).
    Wifi {
        /// Network SSID (unescaped).
        ssid: String,
        /// Security type as declared (`WPA`, `WEP`, `nopass`, …).
        security: String,
        /// Password when present (unescaped).
        password: Option<String>,
        /// Hidden-network flag.
        hidden: bool,
    },
    /// Email (`mailto:` or `MATMSG:`).
    Email {
        /// Recipient address.
        to: String,
        /// Subject as written (not percent-decoded).
        subject: Option<String>,
        /// Body as written (not percent-decoded).
        body: Option<String>,
    },
    /// SMS (`sms:` or `SMSTO:`).
    Sms {
        /// Phone number.
        number: String,
        /// Prefilled body when present.
        body: Option<String>,
    },
    /// Phone call (`tel:`).
    Tel {
        /// Phone number.
        number: String,
    },
    /// Geographic coordinates (`geo:lat,lon`).
    Geo {
        /// Latitude in degrees, validated to [-90, 90].
        lat: f64,
        /// Longitude in degrees, validated to [-180, 180].
        lon: f64,
    },
    /// Contact card (`MECARD:` — the flat DoCoMo format).
    MeCard {
        /// `N:` name as written.
        name: Option<String>,
        /// First `TEL:` value.
        tel: Option<String>,
        /// First `EMAIL:` value.
        email: Option<String>,
        /// First `URL:` value.
        url: Option<String>,
    },
    /// Cryptocurrency payment URI (`bitcoin:` BIP-21 · `ethereum:` ERC-681).
    Crypto {
        /// URI scheme, lowercased (`bitcoin` · `ethereum`).
        scheme: String,
        /// Payment address (ERC-681 `@chain_id` suffix stripped).
        address: String,
        /// BIP-21 `amount` query value when present (display units, kept as
        /// written — ERC-681 `value` is wei notation and is NOT mapped here).
        amount: Option<String>,
    },
    /// Contact card (`BEGIN:VCARD`) — raw, no deep parse.
    VCard {
        /// Full original vCard text.
        raw: String,
    },
    /// Calendar event (`BEGIN:VEVENT`) — raw, no deep parse.
    VEvent {
        /// Full original vEvent text.
        raw: String,
    },
    /// Free text — the fallback class.
    Text,
}

/// Case-insensitive ASCII prefix strip (UTF-8-boundary safe).
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Split `WIFI:` fields on unescaped `;`, unescaping `\;` `\:` `\\` `\"`.
fn wifi_fields(rest: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut segment = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next() {
            Some('\\') => {
                if let Some(escaped) = chars.next() {
                    segment.push(escaped);
                }
            }
            Some(';') | None => {
                if let Some((key, value)) = segment.split_once(':') {
                    fields.push((key.to_owned(), value.to_owned()));
                }
                if segment.is_empty() {
                    break;
                }
                segment.clear();
            }
            Some(c) => segment.push(c),
        }
    }
    fields
}

fn parse_wifi(rest: &str) -> Option<Payload> {
    let mut ssid = None;
    let mut security = None;
    let mut password = None;
    let mut hidden = false;
    for (key, value) in wifi_fields(rest) {
        match key.to_ascii_uppercase().as_str() {
            "S" => ssid = Some(value),
            "T" => security = Some(value),
            "P" => password = Some(value),
            "H" => hidden = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    Some(Payload::Wifi {
        ssid: ssid?,
        security: security.unwrap_or_else(|| "nopass".to_owned()),
        password,
        hidden,
    })
}

/// Extract a query parameter value as written (no percent-decoding).
fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        key.eq_ignore_ascii_case(name).then_some(value)
    })
}

fn parse_mailto(rest: &str) -> Option<Payload> {
    let (to, query) = match rest.split_once('?') {
        Some((to, query)) => (to, Some(query)),
        None => (rest, None),
    };
    if to.is_empty() {
        return None;
    }
    Some(Payload::Email {
        to: to.to_owned(),
        subject: query
            .and_then(|q| query_param(q, "subject"))
            .map(str::to_owned),
        body: query
            .and_then(|q| query_param(q, "body"))
            .map(str::to_owned),
    })
}

fn parse_matmsg(rest: &str) -> Option<Payload> {
    let mut to = None;
    let mut subject = None;
    let mut body = None;
    for segment in rest.split(';') {
        if let Some((key, value)) = segment.split_once(':') {
            match key.to_ascii_uppercase().as_str() {
                "TO" => to = Some(value.to_owned()),
                "SUB" => subject = Some(value.to_owned()),
                "BODY" => body = Some(value.to_owned()),
                _ => {}
            }
        }
    }
    Some(Payload::Email {
        to: to?,
        subject,
        body,
    })
}

fn parse_sms(rest: &str) -> Option<Payload> {
    let (number, body) = match rest.split_once('?') {
        Some((number, query)) => (number, query_param(query, "body").map(str::to_owned)),
        None => (rest, None),
    };
    if number.is_empty() {
        return None;
    }
    Some(Payload::Sms {
        number: number.to_owned(),
        body,
    })
}

fn parse_geo(rest: &str) -> Option<Payload> {
    let coords = rest.split(['?', ';']).next()?;
    let mut parts = coords.split(',');
    let lat: f64 = parts.next()?.parse().ok()?;
    let lon: f64 = parts.next()?.parse().ok()?;
    (lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon))
    .then_some(Payload::Geo { lat, lon })
}

fn parse_mecard(rest: &str) -> Option<Payload> {
    let mut name = None;
    let mut tel = None;
    let mut email = None;
    let mut url = None;
    // MECARD shares the WIFI escape family (\; \: \\) and `;` field split.
    for (key, value) in wifi_fields(rest) {
        if value.is_empty() {
            continue;
        }
        let slot = match key.to_ascii_uppercase().as_str() {
            "N" => &mut name,
            "TEL" => &mut tel,
            "EMAIL" => &mut email,
            "URL" => &mut url,
            _ => continue,
        };
        slot.get_or_insert(value);
    }
    (name.is_some() || tel.is_some() || email.is_some() || url.is_some()).then_some(
        Payload::MeCard {
            name,
            tel,
            email,
            url,
        },
    )
}

fn parse_crypto(scheme: &str, rest: &str) -> Option<Payload> {
    let (addr_part, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    // ERC-681 suffixes the address with `@chain_id`; the address itself
    // never contains `@`.
    let address = addr_part.split('@').next().unwrap_or(addr_part);
    if address.is_empty() {
        return None;
    }
    let amount = query.and_then(|q| {
        q.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == "amount" && !v.is_empty()).then(|| v.to_owned())
        })
    });
    Some(Payload::Crypto {
        scheme: scheme.to_owned(),
        address: address.to_owned(),
        amount,
    })
}

/// Classify decoded QR text into a typed payload. Total: never fails.
pub(crate) fn classify(text: &str) -> Payload {
    if strip_prefix_ci(text, "http://").is_some() || strip_prefix_ci(text, "https://").is_some() {
        return Payload::Url {
            url: text.to_owned(),
        };
    }
    let parsed = if let Some(rest) = strip_prefix_ci(text, "WIFI:") {
        parse_wifi(rest)
    } else if let Some(rest) = strip_prefix_ci(text, "mailto:") {
        parse_mailto(rest)
    } else if let Some(rest) = strip_prefix_ci(text, "MATMSG:") {
        parse_matmsg(rest)
    } else if let Some(rest) = strip_prefix_ci(text, "SMSTO:") {
        let (number, body) = match rest.split_once(':') {
            Some((number, body)) => (number, Some(body.to_owned())),
            None => (rest, None),
        };
        (!number.is_empty()).then(|| Payload::Sms {
            number: number.to_owned(),
            body,
        })
    } else if let Some(rest) = strip_prefix_ci(text, "sms:") {
        parse_sms(rest)
    } else if let Some(rest) = strip_prefix_ci(text, "tel:") {
        (!rest.is_empty()).then(|| Payload::Tel {
            number: rest.to_owned(),
        })
    } else if let Some(rest) = strip_prefix_ci(text, "geo:") {
        parse_geo(rest)
    } else if let Some(rest) = strip_prefix_ci(text, "MECARD:") {
        parse_mecard(rest)
    } else if let Some(rest) = strip_prefix_ci(text, "bitcoin:") {
        parse_crypto("bitcoin", rest)
    } else if let Some(rest) = strip_prefix_ci(text, "ethereum:") {
        parse_crypto("ethereum", rest)
    } else if strip_prefix_ci(text, "BEGIN:VCARD").is_some() {
        Some(Payload::VCard {
            raw: text.to_owned(),
        })
    } else if strip_prefix_ci(text, "BEGIN:VEVENT").is_some() {
        Some(Payload::VEvent {
            raw: text.to_owned(),
        })
    } else {
        None
    };
    parsed.unwrap_or(Payload::Text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn url_http_and_https_case_insensitive() {
        assert_eq!(
            classify("https://qrcode-ai.com/x?y=1"),
            Payload::Url {
                url: "https://qrcode-ai.com/x?y=1".into()
            }
        );
        assert_eq!(
            classify("HTTP://EXAMPLE.com"),
            Payload::Url {
                url: "HTTP://EXAMPLE.com".into()
            }
        );
    }

    #[test]
    fn wifi_with_escaped_semicolon_in_password() {
        let p = classify(r"WIFI:T:WPA;S:my ssid;P:p\;ss;H:true;;");
        assert_eq!(
            p,
            Payload::Wifi {
                ssid: "my ssid".into(),
                security: "WPA".into(),
                password: Some("p;ss".into()),
                hidden: true,
            }
        );
    }

    #[test]
    fn wifi_nopass_minimal() {
        let p = classify("WIFI:S:Cafe Guest;T:nopass;;");
        assert_eq!(
            p,
            Payload::Wifi {
                ssid: "Cafe Guest".into(),
                security: "nopass".into(),
                password: None,
                hidden: false,
            }
        );
    }

    #[test]
    fn wifi_without_ssid_falls_back_to_text() {
        assert_eq!(classify("WIFI:T:WPA;;"), Payload::Text);
    }

    #[test]
    fn mailto_with_subject_and_body() {
        let p = classify("mailto:hello@qrcode-ai.com?subject=Hi%20there&body=Yo");
        assert_eq!(
            p,
            Payload::Email {
                to: "hello@qrcode-ai.com".into(),
                subject: Some("Hi%20there".into()),
                body: Some("Yo".into()),
            }
        );
    }

    #[test]
    fn matmsg_email_form() {
        let p = classify("MATMSG:TO:a@b.com;SUB:Hello;BODY:World;;");
        assert_eq!(
            p,
            Payload::Email {
                to: "a@b.com".into(),
                subject: Some("Hello".into()),
                body: Some("World".into()),
            }
        );
    }

    #[test]
    fn sms_smsto_and_sms_with_body() {
        assert_eq!(
            classify("SMSTO:+33612345678:see you"),
            Payload::Sms {
                number: "+33612345678".into(),
                body: Some("see you".into()),
            }
        );
        assert_eq!(
            classify("sms:+33612345678?body=hello"),
            Payload::Sms {
                number: "+33612345678".into(),
                body: Some("hello".into()),
            }
        );
        assert_eq!(
            classify("sms:+33612345678"),
            Payload::Sms {
                number: "+33612345678".into(),
                body: None,
            }
        );
    }

    #[test]
    fn tel_number() {
        assert_eq!(
            classify("tel:+14155551212"),
            Payload::Tel {
                number: "+14155551212".into()
            }
        );
    }

    #[test]
    fn geo_coordinates_validated() {
        assert_eq!(
            classify("geo:48.8584,2.2945"),
            Payload::Geo {
                lat: 48.8584,
                lon: 2.2945
            }
        );
        // altitude component is ignored
        assert_eq!(
            classify("geo:48.8584,2.2945,35"),
            Payload::Geo {
                lat: 48.8584,
                lon: 2.2945
            }
        );
    }

    #[test]
    fn geo_out_of_range_or_garbage_is_text() {
        assert_eq!(classify("geo:120.0,2.0"), Payload::Text);
        assert_eq!(classify("geo:abc"), Payload::Text);
    }

    #[test]
    fn vcard_and_vevent_kept_raw() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nFN:Ada\nEND:VCARD";
        assert_eq!(classify(vcard), Payload::VCard { raw: vcard.into() });
        let vevent = "BEGIN:VEVENT\nSUMMARY:Launch\nEND:VEVENT";
        assert_eq!(classify(vevent), Payload::VEvent { raw: vevent.into() });
    }

    #[test]
    fn plain_text_is_the_fallback() {
        assert_eq!(classify("just some words"), Payload::Text);
        assert_eq!(classify(""), Payload::Text);
    }

    #[test]
    fn mecard_parses_the_flat_fields() {
        let p = classify("MECARD:N:Doe,John;TEL:+13035551212;EMAIL:john@qrcode-ai.com;URL:https://qrcode-ai.com;;");
        assert_eq!(
            p,
            Payload::MeCard {
                name: Some("Doe,John".into()),
                tel: Some("+13035551212".into()),
                email: Some("john@qrcode-ai.com".into()),
                url: Some("https://qrcode-ai.com".into()),
            }
        );
    }

    #[test]
    fn mecard_with_escaped_semicolon_and_partial_fields() {
        // the MECARD escape family is the WIFI one: \; \: \\
        let p = classify(r"MECARD:N:Acme\; Inc;TEL:555;;");
        assert_eq!(
            p,
            Payload::MeCard {
                name: Some("Acme; Inc".into()),
                tel: Some("555".into()),
                email: None,
                url: None,
            }
        );
    }

    #[test]
    fn mecard_without_any_known_field_is_text() {
        assert_eq!(classify("MECARD:;;"), Payload::Text);
        assert_eq!(classify("MECARD:X:y;;"), Payload::Text);
    }

    #[test]
    fn bitcoin_bip21_with_amount_and_bare() {
        let p = classify("bitcoin:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq?amount=0.01&label=Tip");
        assert_eq!(
            p,
            Payload::Crypto {
                scheme: "bitcoin".into(),
                address: "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq".into(),
                amount: Some("0.01".into()),
            }
        );
        assert_eq!(
            classify("BITCOIN:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"),
            Payload::Crypto {
                scheme: "bitcoin".into(),
                address: "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".into(),
                amount: None,
            }
        );
    }

    #[test]
    fn ethereum_erc681_strips_chain_id_keeps_address() {
        let p = classify("ethereum:0x32Be343B94f860124dC4fEe278FDCBD38C102D88@1?value=2.014e18");
        assert_eq!(
            p,
            Payload::Crypto {
                scheme: "ethereum".into(),
                address: "0x32Be343B94f860124dC4fEe278FDCBD38C102D88".into(),
                amount: None, // ERC-681 `value` is wei-exponent notation, NOT a display amount
            }
        );
    }

    #[test]
    fn crypto_empty_address_is_text() {
        assert_eq!(classify("bitcoin:"), Payload::Text);
        assert_eq!(classify("bitcoin:?amount=1"), Payload::Text);
    }

    #[test]
    fn mecard_and_crypto_wire_names_are_snake_case() {
        let mecard = classify("MECARD:N:Ada;;");
        let json = serde_json::to_string(&mecard).unwrap();
        assert!(json.contains(r#""kind":"me_card""#), "{json}");
        let crypto = classify("bitcoin:addr1");
        let json = serde_json::to_string(&crypto).unwrap();
        assert!(json.contains(r#""kind":"crypto""#), "{json}");
    }

    proptest::proptest! {
        #[test]
        fn classify_never_panics(s in ".*") {
            let _ = classify(&s);
        }
    }
}
