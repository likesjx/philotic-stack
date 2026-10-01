import PhiloticKit
import SwiftUI

@MainActor @Observable
final class CortexStore {
    private let client = NativeCortexClient()
    private var token = ""
    private var generation = UUID()
    var snapshot: CortexSnapshot?
    var memories: [CortexMemory] = []
    var selectedVault = ""
    var nextCursor: String?
    var detail: CortexMemory?
    var error: String?
    var busy = false

    func clear() {
        let previousToken = token
        generation = UUID(); token = ""; snapshot = nil; memories = []
        detail = nil; selectedVault = ""; nextCursor = nil; busy = false; error = nil
        if !previousToken.isEmpty { Task { await client.logout(token: previousToken) } }
    }
    func connect(token: String) async {
        clear(); self.token = token
        await inventory()
    }
    func inventory() async {
        guard !token.isEmpty, !busy else { return }
        let generation = generation
        busy = true
        do {
            let value = try await client.read(CortexSnapshot.self, token: token)
            guard generation == self.generation else { return }
            if let previous = snapshot, previous.cortexID != value.cortexID {
                throw NativeCortexClient.Failure.invalidResponse
            }
            snapshot = value; error = nil
        } catch {
            guard generation == self.generation else { return }
            clear(); self.error = error.localizedDescription
        }
        if generation == self.generation { busy = false }
    }
    func page(vault: String, more: Bool = false) async {
        guard !token.isEmpty, !busy else { return }
        if !more { selectedVault = vault; memories = []; detail = nil; nextCursor = nil }
        let generation = generation
        busy = true
        do {
            let page = try await client.read(CortexMemoryPage.self, token: token,
                vault: vault, cursor: more ? nextCursor : nil)
            guard generation == self.generation else { return }
            guard page.cortexID == snapshot?.cortexID, page.vaultID == selectedVault,
                  page.memories.allSatisfy({ $0.vaultID == selectedVault && !$0.memoryID.isEmpty })
            else { throw NativeCortexClient.Failure.invalidResponse }
            var seen = Set(memories.map(\.id))
            memories.append(contentsOf: page.memories.filter { seen.insert($0.id).inserted })
            nextCursor = page.nextCursor; error = nil
        } catch {
            guard generation == self.generation else { return }
            clear(); self.error = error.localizedDescription
        }
        if generation == self.generation { busy = false }
    }
    func open(_ item: CortexMemory) async {
        guard !token.isEmpty, !busy else { return }
        let generation = generation
        busy = true
        do {
            let value = try await client.read(CortexMemory.self, token: token,
                vault: item.vaultID, memory: item.memoryID)
            guard generation == self.generation else { return }
            guard value.id == item.id else { throw NativeCortexClient.Failure.invalidResponse }
            detail = value; error = nil
        } catch {
            guard generation == self.generation else { return }
            clear(); self.error = error.localizedDescription
        }
        if generation == self.generation { busy = false }
    }
}

struct CortexView: View {
    @State private var store = CortexStore()
    @State private var signIn = NativeCortexSignIn()
    @State private var signingIn = false
    @State private var search = ""
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        List {
            if let snapshot = store.snapshot {
                Section("Cortex · \(snapshot.cortexID)") {
                    Text("Inventory checked \(snapshot.observedAt)").font(.caption)
                    ForEach(snapshot.exclusions, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                    ForEach(snapshot.vaults) { vault in
                        Button { Task { await store.page(vault: vault.id) } } label: {
                            HStack {
                                Label(vault.id, systemImage: "archivebox")
                                Spacer()
                                Text(vault.memoryCount.map(String.init) ?? vault.status.rawValue)
                            }
                        }.disabled(vault.status != .available || store.busy)
                    }
                }
                if !store.selectedVault.isEmpty {
                    Section(store.selectedVault) {
                        TextField("Filter loaded memories", text: $search)
                        Text("Filtering \(store.memories.count) loaded memories. Load more to extend coverage.")
                            .font(.caption).foregroundStyle(.secondary)
                        ForEach(store.memories.filter { search.isEmpty || ($0.concept + " " + $0.content + " " + $0.tags.joined(separator: " ")).localizedCaseInsensitiveContains(search) }) { memory in
                            Button { Task { await store.open(memory) } } label: {
                                VStack(alignment: .leading) {
                                    Text(memory.concept)
                                    Text(memory.content).lineLimit(2).font(.caption).foregroundStyle(.secondary)
                                }
                            }.disabled(store.busy)
                        }
                        if store.nextCursor != nil {
                            Button("Load more") { Task { await store.page(vault: store.selectedVault, more: true) } }.disabled(store.busy)
                        } else if store.memories.isEmpty { Text("No memories in this vault.") }
                    }
                }
                Button("Sign out of Cortex", role: .destructive) { store.clear() }
            } else {
                Section("Connect to Cortex") {
                    Text("Browse your Philotic memory vaults.")
                    Text("Sign in with your administrator account and approve Cortex access. You will be signed out when the app goes into the background.")
                        .font(.caption).foregroundStyle(.secondary)
                    Button(signingIn ? "Signing in…" : "Sign in to Cortex") {
                        signingIn = true; store.error = nil
                        Task {
                            defer { signingIn = false }
                            do {
                                let token = try await signIn.signIn()
                                guard scenePhase != .background else {
                                    await NativeCortexClient().logout(token: token)
                                    return
                                }
                                await store.connect(token: token)
                            } catch is CancellationError {
                                store.error = nil
                            } catch {
                                store.error = "Could not sign in to Cortex. Check your account access and try again."
                            }
                        }
                    }.disabled(store.busy || signingIn)
                    if signingIn { Button("Cancel sign-in") { signIn.cancel() } }
                }
            }
            if store.busy { ProgressView("Reading Cortex…") }
            if let error = store.error { Text(error).foregroundStyle(.orange) }
        }
        .navigationTitle("Cortex")
        .onChange(of: scenePhase) { _, value in
            // The system browser can change scene phase during authentication.
            // No memory or access token is retained during that browser handoff.
            if value == .background { store.clear() }
        }
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                guard !Task.isCancelled else { return }
                if store.snapshot != nil { await store.inventory() }
            }
        }
        .sheet(item: $store.detail) { memory in
            NavigationStack {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        Text(memory.concept).font(.title2)
                        Text(memory.content).textSelection(.enabled)
                        Text("Vault: \(memory.vaultID)\nState: \(memory.state)\nSource: \(memory.source ?? "Not supplied")\nCreated: \(memory.createdAt ?? "Not supplied")\nUpdated: \(memory.updatedAt ?? "Not supplied")")
                            .font(.caption).foregroundStyle(.secondary)
                        Text(memory.tags.joined(separator: " · "))
                    }.padding().frame(maxWidth: .infinity, alignment: .leading)
                }.toolbar { Button("Done") { store.detail = nil } }
            }
            #if os(macOS)
            .frame(minWidth: 520, minHeight: 420)
            #endif
        }
    }
}
