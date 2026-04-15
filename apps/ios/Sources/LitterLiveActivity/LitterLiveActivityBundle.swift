import SwiftUI
import WidgetKit

@main
struct LitterLiveActivityBundle: WidgetBundle {
    var body: some Widget {
        TurnLiveActivity()
        VoiceCallLiveActivity()
    }
}
