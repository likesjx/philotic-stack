/// A UI preference only. Provider eligibility belongs to the authenticated
/// server and must be checked again against complete source lineage at dispatch.
public enum VoiceProviderPreference: String, CaseIterable, Sendable {
    case appleLocal = "apple_local"
    case elevenLabs = "elevenlabs"

    public var displayName: String {
        switch self {
        case .appleLocal: "Apple (on device)"
        case .elevenLabs: "ElevenLabs (preferred)"
        }
    }

    /// Transitional fail-closed boundary: no trusted provider policy receipt
    /// exists on the edge wire yet. Preferences cannot enable external calls.
    public var availableProvider: VoiceProviderPreference { .appleLocal }

    public var availabilityExplanation: String? {
        self == .elevenLabs
            ? "ElevenLabs is unavailable until source privacy can be verified. Replies use the selected Apple voice."
            : nil
    }
}
