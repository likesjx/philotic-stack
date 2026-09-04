import PhiloticKit
import SwiftUI

struct HealthView: View {
    @Bindable var session: ChatSessionManager
    @Bindable var health: HealthKitCaptureService
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var confirmation: HealthShareConfirmation?

    var body: some View {
        Form {
            Section {
                Text(
                    "Review your activity, sleep, and heart-rate summaries for personal wellness. Reading stays on this device until you choose to share."
                )
                if !health.isAvailable {
                    Label(
                        "Apple Health is unavailable here. Use a supported iPhone or iPad. No sample data will be generated or sent.",
                        systemImage: "info.circle")
                }
            }

            Section {
                ForEach(HealthMetric.allCases) { metric in
                    Toggle(isOn: bindingFor(metric)) {
                        Label(metric.displayName, systemImage: metric.systemImage)
                    }
                    .accessibilityIdentifier("health-metric-\(metric.rawValue)")
                }
                Picker("Period", selection: $health.window) {
                    ForEach(HealthReadWindow.allCases) { window in
                        Text(window.title).tag(window)
                    }
                }
            } header: {
                Text("Choose metrics to preview")
            } footer: {
                Text(
                    "These selections choose what to read, not Health permissions. Manage permissions in Apple Health. No background sync and no writes to Health."
                )
            }
            .disabled(!health.isAvailable || health.isBusy)

            Section {
                Button {
                    Task { await health.preparePreview() }
                } label: {
                    Label(
                        health.isReading ? "Reading…" : "Read from Apple Health",
                        systemImage: "heart.text.clipboard")
                }
                .accessibilityIdentifier("health-read")
                .disabled(!health.isAvailable || health.isBusy || health.enabledMetrics.isEmpty)
                if health.isBusy { ProgressView() }
            } footer: {
                Text(
                    "Completed days in your current time zone. Missing readings can mean no samples or no read access; they are never treated as zero."
                )
            }

            if !health.previewItems.isEmpty {
                Section("Preview — daily summaries") {
                    ForEach(health.previewItems) { item in
                        Text(item.observation.evidence.claimSummary)
                            .font(.caption)
                            .textSelection(.enabled)
                    }
                    if health.missingCount > 0 {
                        Text(
                            "\(health.missingCount) metric/day readings were unavailable and are not included."
                        )
                        .foregroundStyle(.secondary)
                    }
                    Button("Discard preview", role: .destructive) { health.discardPreview() }
                        .disabled(health.isBusy)
                }
                Section {
                    Button("Review sharing…") {
                        guard let id = health.previewID,
                            let (url, token) = session.lifeGraphCredentials()
                        else { return }
                        confirmation = HealthShareConfirmation(
                            id: id, baseURL: url, token: token, count: health.previewItems.count)
                    }
                    .accessibilityIdentifier("health-review-sharing")
                    .disabled(
                        health.isBusy || health.previewID == nil
                            || session.lifeGraphCredentials() == nil)
                } footer: {
                    Text(
                        "Sharing stores summaries in your LifeGraph, where agents and their configured AI providers may process them. This is not limited to your current chat. Revoking Health access does not delete previously shared summaries."
                    )
                }
            }

            if session.lifeGraphCredentials() == nil {
                Section { Text("You can preview offline. Connect to a hotel before sharing.") }
            }
            if let status = health.statusMessage {
                Section("Status") { Text(status) }
            }
            if let error = health.lastError {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                }
            }
        }
        .navigationTitle("Health")
        .sheet(item: $confirmation) { request in
            HealthShareConfirmationView(request: request, session: session, health: health)
        }
        .onChange(of: scenePhase) { _, phase in
            // Inactive also occurs during the system permission sheet; clear
            // only on backgrounding so that authorizing reads can complete.
            if phase == .background {
                confirmation = nil
                health.discardPreview()
            }
        }
        #if os(iOS)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        #endif
    }

    private func bindingFor(_ metric: HealthMetric) -> Binding<Bool> {
        Binding(
            get: { health.enabledMetrics.contains(metric) },
            set: { on in
                if on {
                    health.enabledMetrics.insert(metric)
                } else {
                    health.enabledMetrics.remove(metric)
                }
            })
    }
}

private struct HealthShareConfirmation: Identifiable {
    let id: UUID
    let baseURL: URL
    let token: String
    let count: Int

    var destinationDescription: String {
        var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)
        components?.user = nil
        components?.password = nil
        components?.query = nil
        components?.fragment = nil
        return components?.string ?? "Unknown hotel"
    }
}

private struct HealthShareConfirmationView: View {
    let request: HealthShareConfirmation
    @Bindable var session: ChatSessionManager
    @Bindable var health: HealthKitCaptureService
    @Environment(\.dismiss) private var dismiss

    private var destinationUnchanged: Bool {
        guard let (url, token) = session.lifeGraphCredentials() else { return false }
        return url == request.baseURL && token == request.token
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Send \(request.count) health summaries?") {
                    LabeledContent("Hotel", value: request.destinationDescription)
                    Text(
                        "Only the summaries you just previewed will be sent. Your LifeGraph stores them, and agents with access may send them to the AI providers configured by your hotel."
                    )
                    Text(
                        "Share only if you trust this hotel and its configured providers. These summaries support personal wellness, not diagnosis or treatment. Turning off Health access does not retract stored copies."
                    )
                    if !destinationUnchanged {
                        Text("The connection changed. Cancel and review the new destination.")
                    }
                }
                Section {
                    Button("Share these summaries with my agents") {
                        guard destinationUnchanged else { return }
                        Task {
                            await health.sharePreview(
                                id: request.id, baseURL: request.baseURL, bearerToken: request.token
                            )
                            dismiss()
                        }
                    }
                    .disabled(
                        !destinationUnchanged || health.previewID != request.id || health.isBusy)
                    Button("Cancel", role: .cancel) { dismiss() }
                        .disabled(health.isBusy)
                }
            }
            .navigationTitle("Confirm Health Sharing")
            .interactiveDismissDisabled(health.isBusy)
            #if os(macOS)
                .frame(minWidth: 460, minHeight: 400)
            #endif
        }
    }
}

#Preview {
    NavigationStack {
        HealthView(session: ChatSessionManager(), health: HealthKitCaptureService())
    }
}
