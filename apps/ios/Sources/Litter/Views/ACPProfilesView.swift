import SwiftUI

// MARK: - ACP Profiles List

/// Manages ACP provider profiles: view, add, edit, and delete.
/// Profiles capture the remote command needed to launch an ACP-compatible
/// agent over SSH and are used by the agent picker to surface GenericAcp
/// connection options.
struct ACPProfilesView: View {
    @State private var profiles: [ACPProfile] = []
    @State private var showAddSheet = false
    @State private var profileToEdit: ACPProfile?
    @State private var profileToDelete: ACPProfile?
    @State private var showDeleteConfirmation = false

    var body: some View {
        ZStack {
            LitterTheme.backgroundGradient.ignoresSafeArea()
            Form {
                if profiles.isEmpty {
                    emptyStateSection
                } else {
                    profilesSection
                }
                addProfileSection
            }
            .scrollContentBackground(.hidden)
        }
        .navigationTitle("ACP Providers")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(isPresented: $showAddSheet) {
            NavigationStack {
                ACPEditProfileView(mode: .create) { profile in
                    ACPProfileStore.shared.addProfile(profile)
                    reloadProfiles()
                }
            }
        }
        .sheet(item: $profileToEdit) { profile in
            NavigationStack {
                ACPEditProfileView(mode: .edit(profile)) { updated in
                    ACPProfileStore.shared.updateProfile(updated)
                    reloadProfiles()
                }
            }
        }
        .alert("Delete Profile?", isPresented: $showDeleteConfirmation, presenting: profileToDelete) { profile in
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) {
                ACPProfileStore.shared.deleteProfile(id: profile.id)
                reloadProfiles()
            }
        } message: { profile in
            Text("Are you sure you want to delete \"\(profile.displayName)\"? This cannot be undone.")
        }
        .onAppear { reloadProfiles() }
    }

    // MARK: - Empty State

    private var emptyStateSection: some View {
        Section {
            VStack(spacing: 12) {
                Image(systemName: "gearshape.fill")
                    .font(.system(size: 32))
                    .foregroundColor(LitterTheme.textMuted)
                Text("No ACP Providers")
                    .litterFont(.subheadline, weight: .semibold)
                    .foregroundColor(LitterTheme.textPrimary)
                Text("Add a provider profile to connect to custom ACP-compatible agents over SSH.")
                    .litterFont(.caption)
                    .foregroundColor(LitterTheme.textSecondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 24)
            .listRowBackground(Color.clear)
        }
    }

    // MARK: - Profiles List

    private var profilesSection: some View {
        Section {
            ForEach(profiles) { profile in
                Button {
                    profileToEdit = profile
                } label: {
                    profileRow(profile)
                }
                .listRowBackground(LitterTheme.surface.opacity(0.6))
            }
        } header: {
            Text("Profiles")
                .foregroundColor(LitterTheme.textSecondary)
        }
    }

    private func profileRow(_ profile: ACPProfile) -> some View {
        HStack(spacing: 10) {
            Image(systemName: profile.icon ?? "gearshape.fill")
                .foregroundColor(AgentType.genericAcp.tintColor)
                .frame(width: 20)

            VStack(alignment: .leading, spacing: 3) {
                Text(profile.displayName)
                    .litterFont(.subheadline)
                    .foregroundColor(LitterTheme.textPrimary)

                Text(profile.remoteCommand)
                    .litterFont(.caption)
                    .foregroundColor(LitterTheme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)

                HStack(spacing: 4) {
                    Image(systemName: profile.permissionPolicy.icon)
                        .font(.system(size: 9))
                    Text(profile.permissionPolicy.shortName)
                        .font(.system(size: 10))
                }
                .foregroundColor(LitterTheme.textMuted)
            }

            Spacer()

            Button {
                profileToDelete = profile
                showDeleteConfirmation = true
            } label: {
                Image(systemName: "trash")
                    .foregroundColor(LitterTheme.danger)
                    .font(.system(size: 14))
            }
            .buttonStyle(.plain)
        }
    }

    // MARK: - Add Profile

    private var addProfileSection: some View {
        Section {
            Button {
                showAddSheet = true
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "plus.circle.fill")
                        .foregroundColor(LitterTheme.accent)
                        .frame(width: 20)
                    Text("Add Provider")
                        .litterFont(.subheadline)
                        .foregroundColor(LitterTheme.accent)
                }
            }
            .listRowBackground(LitterTheme.surface.opacity(0.6))
        }
    }

    // MARK: - Helpers

    private func reloadProfiles() {
        profiles = ACPProfileStore.shared.profiles()
    }
}

// MARK: - Edit / Create Mode

private enum ACPEditMode {
    case create
    case edit(ACPProfile)
}

// MARK: - ACP Edit Profile View

/// Form for creating or editing an ACP provider profile.
/// Validates that display name and remote command are non-empty before saving.
private struct ACPEditProfileView: View {
    let mode: ACPEditMode
    let onSave: (ACPProfile) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var displayName = ""
    @State private var remoteCommand = ""
    @State private var permissionPolicy: AgentPermissionPolicy = .promptAlways
    @State private var icon: String? = nil

    @State private var showValidationError = false
    @State private var validationError = ""

    private var isEditing: Bool {
        if case .edit = mode { return true }
        return false
    }

    var body: some View {
        ZStack {
            LitterTheme.backgroundGradient.ignoresSafeArea()
            Form {
                nameSection
                commandSection
                permissionSection
                iconSection
            }
            .scrollContentBackground(.hidden)
        }
        .navigationTitle(isEditing ? "Edit Provider" : "New Provider")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("Cancel") { dismiss() }
                    .foregroundColor(LitterTheme.textSecondary)
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Save") { save() }
                    .foregroundColor(LitterTheme.accent)
                    .disabled(!isValid)
            }
        }
        .alert("Validation Error", isPresented: $showValidationError) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(validationError)
        }
        .onAppear { populateFields() }
    }

    // MARK: - Sections

    private var nameSection: some View {
        Section {
            TextField("e.g. My Custom Agent", text: $displayName)
                .litterFont(.subheadline)
                .foregroundColor(LitterTheme.textPrimary)
                .textInputAutocapitalization(.words)
                .autocorrectionDisabled()
        } header: {
            Text("Display Name")
                .foregroundColor(LitterTheme.textSecondary)
        } footer: {
            Text("A short name to identify this provider.")
                .foregroundColor(LitterTheme.textMuted)
        }
    }

    private var commandSection: some View {
        Section {
            TextField("e.g. my-agent --acp", text: $remoteCommand)
                .litterFont(.subheadline)
                .foregroundColor(LitterTheme.textPrimary)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.system(.subheadline, design: .monospaced))
        } header: {
            Text("Remote Command")
                .foregroundColor(LitterTheme.textSecondary)
        } footer: {
            Text("The shell command used to launch the ACP agent over SSH. The command should start an ACP-compatible server on stdio.")
                .foregroundColor(LitterTheme.textMuted)
        }
    }

    private var permissionSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 8) {
                ForEach([
                    AgentPermissionPolicy.autoApproveAll,
                    .autoRejectHighRisk,
                    .promptAlways,
                ], id: \.displayName) { policy in
                    Button {
                        permissionPolicy = policy
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: policy.icon)
                                .foregroundColor(permissionPolicy == policy ? LitterTheme.accent : LitterTheme.textMuted)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(policy.displayName)
                                    .litterFont(.subheadline)
                                    .foregroundColor(LitterTheme.textPrimary)
                                Text(policy.description)
                                    .litterFont(.caption)
                                    .foregroundColor(LitterTheme.textSecondary)
                            }
                            Spacer()
                            if permissionPolicy == policy {
                                Image(systemName: "checkmark")
                                    .foregroundColor(LitterTheme.accent)
                                    .litterFont(.subheadline, weight: .semibold)
                            }
                        }
                    }
                }
            }
            .padding(.vertical, 4)
        } header: {
            Text("Permission Policy")
                .foregroundColor(LitterTheme.textSecondary)
        } footer: {
            Text("How to handle permission requests from this agent.")
                .foregroundColor(LitterTheme.textMuted)
        }
    }

    private var iconSection: some View {
        Section {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    iconOption(nil, label: "Default")
                    iconOption("gearshape.fill", label: "Gear")
                    iconOption("terminal.fill", label: "Terminal")
                    iconOption("cpu", label: "CPU")
                    iconOption("brain", label: "Brain")
                    iconOption("star.fill", label: "Star")
                    iconOption("bolt.fill", label: "Bolt")
                    iconOption("ant.fill", label: "Ant")
                    iconOption("cloud.fill", label: "Cloud")
                    iconOption("network", label: "Network")
                }
            }
            .padding(.vertical, 4)
        } header: {
            Text("Icon")
                .foregroundColor(LitterTheme.textSecondary)
        }
    }

    private func iconOption(_ name: String?, label: String) -> some View {
        Button {
            icon = name
        } label: {
            VStack(spacing: 4) {
                Image(systemName: name ?? "gearshape.fill")
                    .font(.system(size: 20))
                    .foregroundColor(icon == name ? LitterTheme.accent : LitterTheme.textSecondary)
                    .frame(width: 40, height: 40)
                    .background(
                        icon == name
                            ? LitterTheme.accent.opacity(0.2)
                            : LitterTheme.surface.opacity(0.4)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                Text(label)
                    .font(.system(size: 9))
                    .foregroundColor(icon == name ? LitterTheme.accent : LitterTheme.textMuted)
            }
        }
        .buttonStyle(.plain)
    }

    // MARK: - Validation

    private var isValid: Bool {
        !displayName.trimmingCharacters(in: .whitespaces).isEmpty
            && !remoteCommand.trimmingCharacters(in: .whitespaces).isEmpty
            && !containsDangerousShellCharacters(remoteCommand)
    }

    private func containsDangerousShellCharacters(_ command: String) -> Bool {
        // Basic validation: reject commands with shell injection risks
        let dangerous = ["&&", "||", ";", "`", "$(", ">", ">>", "<", "|"]
        return dangerous.contains(where: { command.contains($0) })
    }

    // MARK: - Actions

    private func populateFields() {
        guard case .edit(let profile) = mode else { return }
        displayName = profile.displayName
        remoteCommand = profile.remoteCommand
        permissionPolicy = profile.permissionPolicy
        icon = profile.icon
    }

    private func save() {
        let trimmedName = displayName.trimmingCharacters(in: .whitespaces)
        let trimmedCommand = remoteCommand.trimmingCharacters(in: .whitespaces)

        guard !trimmedName.isEmpty else {
            validationError = "Display name cannot be empty."
            showValidationError = true
            return
        }

        guard !trimmedCommand.isEmpty else {
            validationError = "Remote command cannot be empty."
            showValidationError = true
            return
        }

        if containsDangerousShellCharacters(trimmedCommand) {
            validationError = "Command contains potentially dangerous shell characters (&&, ||, ;, |, etc.). Use a single command."
            showValidationError = true
            return
        }

        let id: UUID
        if case .edit(let existing) = mode {
            id = existing.id
        } else {
            id = UUID()
        }

        let profile = ACPProfile(
            id: id,
            displayName: trimmedName,
            remoteCommand: trimmedCommand,
            permissionPolicy: permissionPolicy,
            icon: icon
        )

        onSave(profile)
        dismiss()
    }
}

// MARK: - Preview

#if DEBUG
#Preview("ACP Profiles") {
    NavigationStack {
        ACPProfilesView()
    }
}

#Preview("ACP Profiles - Empty") {
    NavigationStack {
        ACPProfilesView()
    }
    .onAppear {
        // Clean profiles for empty state preview
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: "litter.acpProfiles")
    }
}
#endif
