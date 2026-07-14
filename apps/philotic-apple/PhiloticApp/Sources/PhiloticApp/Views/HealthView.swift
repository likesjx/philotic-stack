// HealthView.swift
// The Health surface: choose which HealthKit metrics to sync, push daily
// summaries to the LifeGraph as observations, and see the last-sync status.
// On macOS (no HealthKit data) the sync generates plausible sample
// observations so the end-to-end observe path stays demoable.

import PhiloticKit
import SwiftUI

struct HealthView: View {
    @Bindable var session: ChatSessionManager
    @Bindable var health: HealthKitCaptureService
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                if !health.isAvailable {
                    Label(
                        "HealthKit data isn't available on this device. \"Sync\" will send sample observations so you can preview the flow.",
                        systemImage: "info.circle"
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }

            Section("Metrics to sync") {
                ForEach(HealthMetric.allCases) { metric in
                    Toggle(isOn: bindingFor(metric)) {
                        Label(metric.displayName, systemImage: metric.systemImage)
                    }
                }
            }

            Section {
                Button {
                    Task { await sync() }
                } label: {
                    if health.isSyncing {
                        HStack {
                            ProgressView()
                            Text("Syncing…")
                        }
                    } else {
                        Label("Sync Health Now", systemImage: "arrow.up.heart")
                    }
                }
                .disabled(health.isSyncing || session.lifeGraphCredentials() == nil)
            } footer: {
                if session.lifeGraphCredentials() == nil {
                    Text("Connect to a hotel to sync health metrics.")
                } else {
                    Text("Syncs the last 7 days of the selected metrics to your LifeGraph.")
                }
            }

            if let summary = health.lastSyncSummary {
                Section("Status") {
                    LabeledContent("Last sync") {
                        Text(
                            health.lastSyncDate?.formatted(date: .abbreviated, time: .shortened)
                                ?? "—")
                    }
                    Text(summary).font(.caption).foregroundStyle(.secondary)
                }
            }

            if let error = health.lastError {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }
        }
        .navigationTitle("Health")
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
            }
        )
    }

    private func sync() async {
        guard let (baseURL, token) = session.lifeGraphCredentials() else { return }
        await health.syncNow(baseURL: baseURL, bearerToken: token)
    }
}

#Preview {
    NavigationStack {
        HealthView(session: ChatSessionManager(), health: HealthKitCaptureService())
    }
}
