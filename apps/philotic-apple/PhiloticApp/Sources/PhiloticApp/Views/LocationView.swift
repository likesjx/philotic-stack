// LocationView.swift
// Operator-controlled, one-shot location sharing. This surface deliberately
// has no passive/background switch: every LifeGraph update requires a tap.

import SwiftUI

struct LocationView: View {
    @Bindable var session: ChatSessionManager
    @Bindable var location: LocationCaptureService
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                Label(
                    "Share a timestamped snapshot so your agents can use your current place as context.",
                    systemImage: "location.circle"
                )
                .font(.callout)
            } footer: {
                Text("PhiloticApp does not track or upload your location in the background.")
            }

            Section("Sharing precision") {
                Picker("Precision", selection: $location.precision) {
                    ForEach(LocationSharingPrecision.allCases) { precision in
                        Text(precision.title).tag(precision)
                    }
                }
                .pickerStyle(.segmented)

                Text(location.precision.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Permission") {
                LabeledContent("Location access", value: location.authorizationDescription)
            }

            Section {
                Button {
                    Task { await share() }
                } label: {
                    if location.isSharing {
                        HStack {
                            ProgressView()
                            Text("Sharing…")
                        }
                    } else {
                        Label("Share Current Location", systemImage: "location.fill")
                    }
                }
                .disabled(location.isSharing || session.lifeGraphCredentials() == nil)
            } footer: {
                if session.lifeGraphCredentials() == nil {
                    Text("Connect to a hotel before sharing your location.")
                } else {
                    Text("Each tap creates one new snapshot. The observation includes its time and accuracy so agents can tell when it is stale.")
                }
            }

            if let summary = location.lastSharedSummary {
                Section("Last shared") {
                    if let date = location.lastSharedAt {
                        LabeledContent("Observed") {
                            Text(date.formatted(date: .abbreviated, time: .shortened))
                        }
                    }
                    Text(summary)
                        .font(.caption)
                        .textSelection(.enabled)
                }
            }

            if let error = location.lastError {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }
        }
        .navigationTitle("Location")
        #if os(iOS)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        #endif
    }

    private func share() async {
        guard let (baseURL, token) = session.lifeGraphCredentials() else { return }
        await location.shareCurrentLocation(baseURL: baseURL, bearerToken: token)
    }
}

#Preview {
    NavigationStack {
        LocationView(session: ChatSessionManager(), location: LocationCaptureService())
    }
}
