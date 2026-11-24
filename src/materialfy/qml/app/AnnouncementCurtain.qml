import QtQuick
import Md3

// Live Activity Announcement Curtain ("шторка")
// Шейдерный wipe из wipe.frag.qsb, плавно открывающий новый трек и исполнителя,
// синхронизированный со световым лучом от обложки слева направо.
Item {
    id: root

    property Item cmpCover: null
    property bool motion: true
    property real progress: 0
    property real dir: 1
    property real mode: 0
    property real edge: 0.22
    property real skew: 0.12
    property point origin: cmpCover ? Qt.point(cmpCover.x + cmpCover.width / 2, cmpCover.y + cmpCover.height / 2)
                                    : Qt.point(22, 22)
    property real shown: (root.visible && (wipeAnim.running || !root.motion)) ? 1.0 : 0.0
    property real aspect: width / Math.max(1, height)
    property real seed: 0
    readonly property bool running: wipeAnim.running

    property string prevTitle: ""
    property string prevArtist: ""
    property color ink: "#ffffff"
    property color inkDim: Qt.rgba(1, 1, 1, 0.70)
    property color accent: Theme.primary
    property bool colorMode: false

    readonly property alias anim: wipeAnim
    readonly property real beamX: (progress * (1.0 + edge + skew) - edge * 0.5 - skew * 0.5) * width

    function trigger(title, artist) {
        if (title !== undefined && title !== "") prevTitle = title
        if (artist !== undefined) prevArtist = artist
        if (!motion) {
            wipeAnim.stop()
            progress = 1.0
            return
        }
        wipeAnim.stop()
        progress = 0
        snap.scheduleUpdate()
        wipeAnim.restart()
    }

    function stop() {
        wipeAnim.stop()
        progress = 1.0
    }

    // Снимок предыдущего состояния шторки с прежним названием и исполнителем
    Item {
        id: oldContent
        anchors.fill: parent
        visible: false

        Item {
            x: root.cmpCover ? (root.cmpCover.x + root.cmpCover.width + 10) : 46
            y: (parent.height - oldCol.height) / 2
            width: Math.max(10, parent.width - x - 42)
            height: oldCol.height
            clip: true

            Column {
                id: oldCol
                width: parent.width
                spacing: 1

                Label {
                    width: parent.width
                    text: root.prevTitle
                    type: "labelMedium"
                    weight: 600
                    color: root.ink
                    elide: Text.ElideRight
                }
                Label {
                    width: parent.width
                    text: root.prevArtist
                    type: "labelSmall"
                    color: root.inkDim
                    elide: Text.ElideRight
                }
            }
        }

        Rectangle {
            anchors.fill: parent
            radius: height / 2
            color: "transparent"
            border.width: 1
            border.color: Qt.rgba(1, 1, 1, 0.06)
        }
    }

    ShaderEffectSource {
        id: snap
        sourceItem: oldContent
        live: false
        hideSource: true
        visible: false
    }

    // Реальный wipe шейдер MD3 (wipe.frag.qsb)
    ShaderEffect {
        id: fx
        anchors.fill: parent
        visible: root.running
        property variant source: snap
        property real progress: root.progress
        property real dir: root.dir
        property real edge: root.edge
        property real skew: root.skew
        property real shown: root.shown
        property real mode: root.mode
        property real aspect: width / Math.max(1, height)
        property real seed: root.seed
        property point origin: root.origin
        fragmentShader: Qt.resolvedUrl("../Md3/shaders/wipe.frag.qsb")
    }

    // Световой луч, синхронизированный с фронтом шейдера
    Rectangle {
        id: beam
        y: 0
        height: parent.height
        width: 76
        x: root.beamX - width / 2
        radius: height / 2
        visible: root.running && root.motion
        antialiasing: true
        clip: true
        gradient: Gradient {
            orientation: Gradient.Horizontal
            GradientStop { position: 0.0; color: "transparent" }
            GradientStop {
                position: 0.4
                color: Qt.rgba(root.accent.r, root.accent.g, root.accent.b, root.colorMode ? 0.32 : 0.20)
            }
            GradientStop {
                position: 0.7
                color: Qt.rgba(1, 1, 1, root.colorMode ? 0.38 : 0.24)
            }
            GradientStop { position: 1.0; color: "transparent" }
        }
    }

    NumberAnimation {
        id: wipeAnim
        target: root
        property: "progress"
        from: 0
        to: 1
        duration: root.motion ? 650 : 0
        easing.type: Easing.BezierSpline
        easing.bezierCurve: Theme.motion.emphasizedDecelerate
        onFinished: root.progress = 1.0
    }
}
