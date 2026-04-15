import SwiftUI

struct InlineVoiceStatusStrip: View {
    let session: VoiceSessionState
    let agentLabel: String
    let onToggleSpeaker: () -> Void

    private var inputLevel: Float {
        session.isListening ? max(0.08, session.scaledInputLevel) : max(0, session.scaledInputLevel)
    }

    private var outputLevel: Float {
        session.isSpeaking ? max(0.08, session.scaledOutputLevel) : max(0, session.scaledOutputLevel)
    }

    var body: some View {
        HStack(spacing: 8) {
            HStack(spacing: 5) {
                Circle()
                    .fill(session.isListening ? LitterTheme.accent : LitterTheme.textMuted.opacity(0.4))
                    .frame(width: 5, height: 5)
                Text("YOU")
                    .font(LitterFont.monospaced(.caption2, weight: .bold))
                    .foregroundColor(session.isListening ? LitterTheme.textPrimary : LitterTheme.textMuted)
                AudioWaveformView(level: inputLevel, tint: LitterTheme.accent)
                    .frame(width: 48, height: 14)
            }

            HStack(spacing: 5) {
                Circle()
                    .fill(session.isSpeaking ? LitterTheme.warning : LitterTheme.textMuted.opacity(0.4))
                    .frame(width: 5, height: 5)
                Text(agentLabel)
                    .font(LitterFont.monospaced(.caption2, weight: .bold))
                    .foregroundColor(session.isSpeaking ? LitterTheme.textPrimary : LitterTheme.textMuted)
                AudioWaveformView(level: outputLevel, tint: LitterTheme.warning)
                    .frame(width: 48, height: 14)
            }

            Spacer()

            Button(action: onToggleSpeaker) {
                HStack(spacing: 4) {
                    Image(systemName: session.route.iconName)
                        .font(.system(size: 10, weight: .semibold))
                    Text(session.route.label)
                        .font(LitterFont.styled(.caption2, weight: .semibold))
                }
                .foregroundColor(session.route.supportsSpeakerToggle ? LitterTheme.textPrimary : LitterTheme.textMuted)
            }
            .buttonStyle(.plain)
            .disabled(!session.route.supportsSpeakerToggle)

            Text(session.phase.displayTitle)
                .font(LitterFont.monospaced(.caption2, weight: .medium))
                .foregroundColor(phaseColor(session.phase))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .background(LitterTheme.surface.opacity(0.6))
    }

    private func phaseColor(_ phase: VoiceSessionPhase) -> Color {
        switch phase {
        case .connecting, .thinking, .handoff:
            return LitterTheme.warning
        case .listening, .speaking:
            return LitterTheme.accent
        case .error:
            return LitterTheme.danger
        }
    }
}
