import SwiftUI

/// A projection of the existing session and LifeGraph cache, not a second store.
struct CompanionDashboard: View {
    @Bindable var session: ChatSessionManager
    @Bindable var router: CompanionRouter
    var compact = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(Date.now, format: .dateTime.weekday(.wide).month().day())
                        .font(.caption.weight(.medium)).foregroundStyle(.secondary)
                    Text("A little more present.")
                        .font(compact ? .title2.bold() : .largeTitle.bold())
                    Text("Your agents. Your context. Your call.").foregroundStyle(.secondary)
                }
                Button { router.open(.agents) } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "waveform").font(.title2)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("What's on your mind?").font(.headline)
                            Text("Continue with your Philotic agents").font(.caption)
                        }
                        Spacer()
                        Image(systemName: "arrow.up.right")
                    }
                    .padding(18).frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.borderedProminent).tint(.indigo)

                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Label("From your LifeGraph", systemImage: "brain").font(.headline)
                        Spacer()
                        Button("Explore") { router.open(.life) }.font(.caption)
                    }
                    Text(session.lifeGraph.selectedLens.title).font(.caption).foregroundStyle(.secondary)
                    if session.lifeGraphCredentials() == nil {
                        Text("Connect to see your context.").foregroundStyle(.secondary)
                        Button("Connection settings") { router.sheet = .settings }
                    } else if session.lifeGraph.isLoading {
                        ProgressView("Recalling…")
                    } else if let error = session.lifeGraph.lastError {
                        Label("Could not refresh", systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                        Text(error).font(.caption).lineLimit(3)
                        Button("Retry") { Task { await refresh() } }
                    } else if session.lifeGraph.lastRefreshed == nil {
                        Button("Load my context") { Task { await refresh() } }
                    } else if session.lifeGraph.packets.isEmpty {
                        Text("Nothing surfaced in this lens.").foregroundStyle(.secondary)
                    } else {
                        ForEach(session.lifeGraph.packets.prefix(compact ? 2 : 3)) { ranked in
                            Button { router.open(.life) } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(ranked.packet.claimSummary).lineLimit(2)
                                    Text("\(ranked.packet.claimRef.label) · \(ranked.packet.validationState)")
                                        .font(.caption2).foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }.buttonStyle(.plain)
                        }
                    }
                    if let date = session.lifeGraph.lastRefreshed {
                        Text("Last refreshed \(date.formatted(date: .omitted, time: .shortened))")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                }
                .padding(18)
                .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 20))

                VStack(alignment: .leading, spacing: 12) {
                    Text("Apple connections").font(.headline)
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: compact ? 160 : 150))], spacing: 12) {
                        connection("Health", detail: "Review, then share", icon: "heart.fill", color: .pink, sheet: .health)
                        connection("Places", detail: "One snapshot at a time", icon: "map.fill", color: .teal, sheet: .location)
                        connection("Reminders", detail: "Read-only · on this device", icon: "checklist", color: .orange, sheet: .reminders)
                        connection("On-device AI", detail: "Apple Intelligence", icon: "sparkles", color: .purple, sheet: .intelligence)
                    }
                }
                Label("Apple permissions never imply permission to share with agents.", systemImage: "hand.raised")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .padding(compact ? 16 : 24).frame(maxWidth: 760).frame(maxWidth: .infinity)
        }
        .refreshable { await refresh() }.navigationTitle("Today")
    }

    private func connection(_ title: String, detail: String, icon: String, color: Color, sheet: CompanionSheet) -> some View {
        Button { router.sheet = sheet } label: {
            VStack(alignment: .leading, spacing: 10) {
                Image(systemName: icon).font(.title2).foregroundStyle(color)
                Text(title).font(.headline).foregroundStyle(.primary)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, minHeight: 100, alignment: .leading).padding(14)
            .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 18))
        }.buttonStyle(.plain)
    }

    private func refresh() async {
        guard let (url, token) = session.lifeGraphCredentials(), !session.lifeGraph.isLoading else { return }
        await session.lifeGraph.refresh(baseURL: url, bearerToken: token)
    }
}
