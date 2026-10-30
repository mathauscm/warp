use super::*;

#[test]
fn providers_round_trip_through_their_persisted_names() {
    for provider in ChatProvider::ALL {
        assert_eq!(ChatProvider::parse(provider.as_str()), Some(provider));
    }
    assert_eq!(ChatProvider::parse("gemini"), None);
    assert_eq!(ChatProvider::default(), ChatProvider::Claude);
}
