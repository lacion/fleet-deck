import QtQuick
import Quickshell.Io
import qs.Commons as Commons
import "Model.js" as Model

// The feed of the bar widget. It runs `fleetdeck desktop feed --notify`, reads
// one JSON line at a time, bounds each line with Model.parse and keeps the
// bounded summary. A non-zero exit restarts the feed after 5 s. Exit 0 is final.
// A second process runs the ambient screensaver job with the same restart rule.
// The Process and SplitParser types are the ones of the reference plugin; the
// live smoke of omarchy-p4.md section 5 confirms them on the owner machines.
Item {
  id: root

  property var summary: Model.unknownState()
  readonly property string deck: summary.deck
  readonly property var counts: summary.counts
  readonly property var needsYou: summary.needs_you
  readonly property var usage: summary.usage
  readonly property var stations: summary.stations

  function handleLine(line) {
    root.summary = Model.parse(line)
  }

  Timer {
    id: restartTimer
    interval: 5000
    repeat: false
    onTriggered: feed.running = true
  }

  Process {
    id: feed
    running: true
    command: ["fleetdeck", "desktop", "feed", "--notify"]
    stdout: SplitParser {
      onRead: function(data) { root.handleLine(data) }
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        root.summary = Model.unknownState()
        restartTimer.start()
      }
    }
  }

  Timer {
    id: screensaverTimer
    interval: 5000
    repeat: false
    onTriggered: screensaver.running = true
  }

  Process {
    id: screensaver
    running: true
    command: ["fleetdeck", "desktop", "screensaver-run"]
    onExited: function(exitCode) {
      if (exitCode !== 0) {
        screensaverTimer.start()
      }
    }
  }
}
