import PhiloticKit
import SwiftUI

/// Topic tiles are explicit tag membership, not made-up semantic graph edges.
struct CortexWorkspaceView: View {
    @Bindable var store: CortexStore
    let snapshot: CortexSnapshot
    @State private var search = ""
    @State private var topic: String?
    private let columns = [GridItem(.adaptive(minimum: 150), spacing: 12)]

    private var topics: [CortexTopic] { CortexTopic.project(store.memories) }
    private var visible: [CortexMemory] {
        let members = topic.flatMap { tag in topics.first { $0.tag == tag } }.map { Set($0.members) }
        return store.memories.filter { memory in
            (members?.contains(memory.id) ?? true) && (search.isEmpty ||
                (memory.concept + " " + memory.content + " " + memory.tags.joined(separator: " "))
                    .localizedCaseInsensitiveContains(search))
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Label("Your memory workspace", systemImage: "brain.head.profile").font(.title2.bold())
            Text("\(snapshot.cortexID) · \(snapshot.vaults.count) authorized vaults")
                .font(.caption).foregroundStyle(.secondary)
            LazyVGrid(columns: columns, spacing: 12) {
                ForEach(snapshot.vaults) { vault in
                    Button { Task { await store.page(vault: vault.id) } } label: {
                        VStack(alignment: .leading, spacing: 10) {
                            Image(systemName: "archivebox.fill").font(.title2)
                            Text(vault.id).font(.headline).lineLimit(2)
                            Text(vault.memoryCount.map { "\($0) memories" } ?? "Count unavailable")
                                .font(.caption)
                            if vault.status != .available { Text(vault.status.rawValue).font(.caption) }
                        }.frame(maxWidth: .infinity, minHeight: 110, alignment: .leading)
                            .padding(12)
                            .background(store.selectedVault == vault.id ? Color.accentColor.opacity(0.16) : Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 16))
                            .contentShape(RoundedRectangle(cornerRadius: 16))
                    }.buttonStyle(.plain).disabled(vault.status != .available || store.busy)
                        .accessibilityAddTraits(store.selectedVault == vault.id ? .isSelected : [])
                }
            }
            ForEach(snapshot.exclusions, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
            if !store.selectedVault.isEmpty {
                Divider()
                Text("Explore \(store.selectedVault)").font(.title3.bold())
                Text("\(store.memories.count) loaded memories · topics reflect their stored tags, not Muninn association edges.")
                    .font(.caption).foregroundStyle(.secondary)
                TextField("Find in loaded memories", text: $search).textFieldStyle(.roundedBorder)
                if !topics.isEmpty {
                    ScrollView(.horizontal) {
                        HStack(spacing: 10) {
                            topicButton("All topics", value: nil, count: store.memories.count)
                            ForEach(topics) { item in
                                topicButton(item.tag, value: item.tag, count: item.members.count)
                            }
                        }.padding(.vertical, 4)
                    }
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 250))], spacing: 12) {
                    ForEach(visible) { memory in
                        Button { Task { await store.open(memory) } } label: {
                            VStack(alignment: .leading, spacing: 10) {
                                Label(memory.state, systemImage: "sparkle").font(.caption).foregroundStyle(.secondary)
                                Text(memory.concept).font(.headline).lineLimit(3)
                                Text(memory.content).font(.subheadline).foregroundStyle(.secondary).lineLimit(4)
                                Text(memory.tags.prefix(3).joined(separator: " · ")).font(.caption2).foregroundStyle(.tint)
                            }.frame(maxWidth: .infinity, minHeight: 140, alignment: .topLeading)
                                .padding(16).background(.thinMaterial, in: RoundedRectangle(cornerRadius: 16))
                                .contentShape(RoundedRectangle(cornerRadius: 16))
                        }.buttonStyle(.plain).disabled(store.busy)
                    }
                }
                if visible.isEmpty && !store.busy {
                    ContentUnavailableView("No loaded matches", systemImage: "magnifyingglass",
                        description: Text("Clear the filters or load more memories."))
                }
                if store.nextCursor != nil {
                    Button("Load more memories", systemImage: "arrow.down.circle") {
                        Task { await store.page(vault: store.selectedVault, more: true) }
                    }.disabled(store.busy)
                }
            }
        }
        .onChange(of: store.selectedVault) { _, _ in topic = nil; search = "" }
    }

    private func topicButton(_ title: String, value: String?, count: Int) -> some View {
        Button { topic = value } label: {
            VStack(spacing: 4) {
                Text(title).font(.subheadline.bold())
                Text("\(count)").font(.title3.monospacedDigit())
            }.padding(12)
                .background(topic == value ? Color.accentColor.opacity(0.2) : Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
        }.buttonStyle(.plain).accessibilityAddTraits(topic == value ? .isSelected : [])
    }
}
