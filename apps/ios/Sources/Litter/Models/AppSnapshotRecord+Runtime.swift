import Foundation

extension AppSnapshotRecord {
    func threadHasTrackedTurn(for key: ThreadKey) -> Bool {
        guard let thread = threadSnapshot(for: key) else { return false }
        if thread.hasActiveTurn {
            return true
        }

        if pendingApprovals.contains(where: {
            $0.serverId == key.serverId && $0.threadId == key.threadId
        }) {
            return true
        }

        return pendingUserInputs.contains(where: {
            $0.serverId == key.serverId && $0.threadId == key.threadId
        })
    }

    var threadsWithTrackedTurns: [AppThreadSnapshot] {
        threads.filter { threadHasTrackedTurn(for: $0.key) }
    }
}
