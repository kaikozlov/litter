import ActivityKit
import SwiftUI
import WidgetKit

// MARK: - Colors

private let amberColor   = LitterPalette.amber
private let dangerColor  = LitterPalette.dangerFixed

struct CodexTurnLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: CodexTurnAttributes.self) { context in
            lockScreenView(context: context)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    litterLogo(size: 20)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.attributes.prompt)
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundStyle(.white)
                        .lineLimit(1)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    liveTimer(context: context, size: 12)
                        .foregroundStyle(.white.opacity(0.4))
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack(spacing: 6) {
                        phaseBadge(context.state)
                        Text(context.attributes.model)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.white.opacity(0.3))
                        Spacer()
                        if context.state.fileChangeCount > 0 {
                            Label("\(context.state.fileChangeCount)", systemImage: "doc.text")
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(.white.opacity(0.3))
                        }
                        if context.state.toolCallCount > 0 {
                            Label("\(context.state.toolCallCount)", systemImage: "chevron.left.forwardslash.chevron.right")
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(.white.opacity(0.3))
                        }
                        if context.state.contextPercent > 0 {
                            ctxBadge(context.state.contextPercent)
                        }
                    }
                }
            } compactLeading: {
                litterLogo(size: 16)
            } compactTrailing: {
                liveTimer(context: context, size: 12)
                    .foregroundStyle(.white.opacity(0.5))
            } minimal: {
                litterLogo(size: 16)
            }
        }
    }

    // MARK: - Lock Screen

    private func lockScreenView(context: ActivityViewContext<CodexTurnAttributes>) -> some View {
        LockScreenCardView(
            prompt: context.attributes.prompt,
            model: context.attributes.model,
            cwd: context.attributes.cwd,
            state: context.state,
            timerContent: AnyView(
                liveTimer(context: context, size: 15)
                    .fontWeight(.regular)
                    .foregroundStyle(isActive(context.state) ? .white.opacity(0.7) : .white.opacity(0.45))
            )
        )
    }

    // MARK: - Components

    private func litterLogo(size: CGFloat) -> some View {
        Image("brand_logo")
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(width: size, height: size)
    }

    private func displayText(_ state: CodexTurnAttributes.ContentState) -> String {
        if let snippet = state.outputSnippet, !snippet.isEmpty {
            return snippet
        }
        return statusText(state)
    }

    private func snippetColor(_ state: CodexTurnAttributes.ContentState) -> Color {
        if state.outputSnippet != nil {
            return .white.opacity(0.35)
        }
        switch state.phase {
        case .thinking, .toolCall: return amberColor.opacity(0.6)
        case .completed: return .white.opacity(0.35)
        case .failed: return dangerColor.opacity(0.6)
        }
    }

    private func phaseBadge(_ state: CodexTurnAttributes.ContentState) -> some View {
        Text(phaseBadgeText(state))
            .font(.system(size: 10, weight: .medium, design: .monospaced))
            .foregroundStyle(phaseColor(state))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(phaseBgColor(state))
            )
    }

    private func metaChip(systemImage: String, text: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: systemImage)
                .font(.system(size: 9))
                .foregroundStyle(.white.opacity(0.25))
            Text(text)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.white.opacity(0.3))
                .lineLimit(1)
        }
        .padding(.trailing, 10)
    }

    private func ctxBadge(_ percent: Int) -> some View {
        Text("\(percent)%")
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .foregroundStyle(ctxColor(percent))
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(ctxBgColor(percent))
            )
    }

    @ViewBuilder
    private func liveTimer(context: ActivityViewContext<CodexTurnAttributes>, size: CGFloat) -> some View {
        if isActive(context.state) {
            Text(timerInterval: context.attributes.startDate...Date.distantFuture, countsDown: false)
                .font(.system(size: size, design: .monospaced))
                .monospacedDigit()
                .multilineTextAlignment(.trailing)
        } else {
            Text(formatElapsed(context.state.elapsedSeconds))
                .font(.system(size: size, design: .monospaced))
                .monospacedDigit()
                .multilineTextAlignment(.trailing)
        }
    }

    // MARK: - Helpers

    private func isActive(_ state: CodexTurnAttributes.ContentState) -> Bool {
        state.phase == .thinking || state.phase == .toolCall
    }

    private func statusText(_ state: CodexTurnAttributes.ContentState) -> String {
        switch state.phase {
        case .thinking: return "Thinking..."
        case .toolCall: return state.toolName ?? "Running tool..."
        case .completed: return "Done"
        case .failed: return "Failed"
        }
    }

    private func phaseBadgeText(_ state: CodexTurnAttributes.ContentState) -> String {
        switch state.phase {
        case .thinking: return "thinking"
        case .toolCall: return "tool"
        case .completed: return "done"
        case .failed: return "failed"
        }
    }

    private func phaseColor(_ state: CodexTurnAttributes.ContentState) -> Color {
        switch state.phase {
        case .thinking, .toolCall: return amberColor
        case .completed: return .white.opacity(0.5)
        case .failed: return dangerColor
        }
    }

    private func phaseBgColor(_ state: CodexTurnAttributes.ContentState) -> Color {
        switch state.phase {
        case .thinking, .toolCall: return amberColor.opacity(0.12)
        case .completed: return .white.opacity(0.06)
        case .failed: return dangerColor.opacity(0.12)
        }
    }

    private func ctxColor(_ percent: Int) -> Color {
        if percent >= 80 { return dangerColor }
        if percent >= 60 { return amberColor }
        return .white.opacity(0.35)
    }

    private func ctxBgColor(_ percent: Int) -> Color {
        if percent >= 80 { return dangerColor.opacity(0.1) }
        if percent >= 60 { return amberColor.opacity(0.1) }
        return .white.opacity(0.05)
    }

    private func formatElapsed(_ seconds: Int) -> String {
        let m = seconds / 60
        let s = seconds % 60
        return String(format: "%d:%02d", m, s)
    }

    private func cwdShortened(_ cwd: String) -> String {
        guard !cwd.isEmpty else { return "~" }
        if let last = cwd.split(separator: "/").last { return String(last) }
        return cwd
    }
}

// MARK: - Lock Screen Card (extracted for previews)

struct LockScreenCardView: View {
    let prompt: String
    let model: String
    let cwd: String
    let state: CodexTurnAttributes.ContentState
    let timerContent: AnyView

    @Environment(\.colorScheme) private var colorScheme

    // All colors derived from the shared LitterPalette
    private var cardBackground: Color { LitterPalette.surface.color(for: colorScheme) }
    private var logoBackground: Color { LitterPalette.surfaceLight.color(for: colorScheme) }
    private var primaryText: Color { LitterPalette.textPrimary.color(for: colorScheme) }
    private var secondaryText: Color { LitterPalette.textSecondary.color(for: colorScheme) }
    private var tertiaryText: Color { LitterPalette.textMuted.color(for: colorScheme) }
    private var mutedText: Color { tertiaryText.opacity(0.7) }
    private var chipBgBase: Color { colorScheme == .dark ? .white : .black }
    private var completedBadgeFg: Color { secondaryText }
    private var dangerText: Color { LitterPalette.danger.color(for: colorScheme) }
    private var warningText: Color { LitterPalette.warning.color(for: colorScheme) }
    private var successText: Color { LitterPalette.success.color(for: colorScheme) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Row 1: logo + prompt + timer
            HStack(alignment: .center, spacing: 10) {
                Image("brand_logo")
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
                    .frame(width: 24, height: 24)
                    .padding(2)
                    .background(
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .fill(logoBackground)
                    )

                Text(prompt)
                    .font(.system(size: 13, weight: .medium, design: .monospaced))
                    .foregroundStyle(primaryText)
                    .lineLimit(1)

                Spacer(minLength: 0)

                timerContent
                    .frame(width: 52, alignment: .trailing)
            }

            // Row 2: snippet
            Text(displayText)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(snippetColor)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 4)
                .padding(.leading, 38)

            // Row 3: meta chips
            HStack(spacing: 0) {
                phaseBadge

                Text(model)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(tertiaryText)
                    .padding(.leading, 8)

                Spacer(minLength: 4)

                if state.fileChangeCount > 0 {
                    metaChip(systemImage: "doc.text", text: "\(state.fileChangeCount)")
                }
                if state.toolCallCount > 0 {
                    metaChip(systemImage: "chevron.left.forwardslash.chevron.right", text: "\(state.toolCallCount)")
                }
                if let pushCount = state.pushCount, pushCount > 0 {
                    metaChip(systemImage: "antenna.radiowaves.left.and.right", text: "\(pushCount)")
                }

                metaChip(systemImage: "folder", text: cwdShortened)

                if state.contextPercent > 0 {
                    ctxBadge
                        .padding(.leading, 4)
                }
            }
            .padding(.top, 8)
            .padding(.leading, 38)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(cardBackground)
    }

    // MARK: - Computed properties

    private var displayText: String {
        if let snippet = state.outputSnippet, !snippet.isEmpty { return snippet }
        switch state.phase {
        case .thinking: return "Thinking..."
        case .toolCall: return state.toolName ?? "Running tool..."
        case .completed: return "Done"
        case .failed: return "Failed"
        }
    }

    private var snippetColor: Color {
        if state.outputSnippet != nil { return secondaryText }
        switch state.phase {
        case .thinking, .toolCall: return warningText.opacity(0.7)
        case .completed: return secondaryText
        case .failed: return dangerText.opacity(0.7)
        }
    }

    private var phaseBadge: some View {
        let text: String = {
            switch state.phase {
            case .thinking: return "thinking"
            case .toolCall: return "tool"
            case .completed: return "done"
            case .failed: return "failed"
            }
        }()
        let fg: Color = {
            switch state.phase {
            case .thinking, .toolCall: return warningText
            case .completed: return completedBadgeFg
            case .failed: return dangerText
            }
        }()
        let bg: Color = {
            switch state.phase {
            case .thinking, .toolCall: return warningText.opacity(colorScheme == .dark ? 0.12 : 0.15)
            case .completed: return chipBgBase.opacity(0.06)
            case .failed: return dangerText.opacity(colorScheme == .dark ? 0.12 : 0.15)
            }
        }()
        return Text(text)
            .font(.system(size: 10, weight: .medium, design: .monospaced))
            .foregroundStyle(fg)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(RoundedRectangle(cornerRadius: 4, style: .continuous).fill(bg))
    }

    private func metaChip(systemImage: String, text: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: systemImage)
                .font(.system(size: 9))
                .foregroundStyle(mutedText)
            Text(text)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(tertiaryText)
                .lineLimit(1)
        }
        .padding(.trailing, 10)
    }

    private var ctxBadge: some View {
        let p = state.contextPercent
        let fg: Color = p >= 80 ? dangerText : p >= 60 ? warningText : tertiaryText
        let bg: Color = p >= 80 ? dangerText.opacity(0.1) : p >= 60 ? warningText.opacity(0.1) : chipBgBase.opacity(0.05)
        return Text("\(p)%")
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .foregroundStyle(fg)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(RoundedRectangle(cornerRadius: 4, style: .continuous).fill(bg))
    }

    private var cwdShortened: String {
        guard !cwd.isEmpty else { return "~" }
        if let last = cwd.split(separator: "/").last { return String(last) }
        return cwd
    }
}

