import QtQuick
import QtQuick.Layouts
import Md3
import Materialfy.Core

// Настройки → Внешний вид: где жить плееру вне окна и каким ему быть. Крупные карточки с живым превью
// (макет docs/design/50-dynamic-island.html): выключен / островок / панель задач (/ оба — Windows), ниже —
// стиль (чёрный / цветной) тоже с превью. Выбранная карточка — тональный контейнер, углы 16 → 28, галочка.
ColumnLayout {
    id: root

    // режимы, доступные на этой системе (SettingsPage: островок — Windows и X11, панель задач — Windows)
    property var modes: ["off", "island", "taskbar"]
    readonly property bool active: Settings.floatingPlayer !== "off" && modes.indexOf(Settings.floatingPlayer) >= 0

    spacing: 12
    Layout.fillWidth: true

    readonly property var _modeInfo: ({
        off: { title: I18n.t("Off"), sub: I18n.t("Only the Materialfy window") },
        island: { title: I18n.t("Island"), sub: I18n.t("At the top of the screen, like a notch") },
        taskbar: { title: I18n.t("Taskbar"), sub: I18n.t("Inside the Windows taskbar") },
        both: { title: I18n.t("Both"), sub: I18n.t("Island and taskbar together") }
    })

    Label {
        text: I18n.t("Floating player")
        type: "bodyLarge"
    }
    Label {
        Layout.fillWidth: true
        Layout.topMargin: -8
        text: I18n.t("The track and controls outside the window, even when it’s minimized or in the tray. Point at it and use the arrow keys; the mouse wheel changes the volume")
        type: "bodyMedium"
        color: Theme.fgSurfaceVariant
        wrapMode: Text.Wrap
    }
    Flow {
        Layout.fillWidth: true
        spacing: 10
        Repeater {
            model: root.modes
            Choice {
                required property string modelData
                title: root._modeInfo[modelData].title
                sub: root._modeInfo[modelData].sub
                selected: Settings.floatingPlayer === modelData
                onPicked: Settings.floatingPlayer = modelData
                MiniDesk {
                    anchors.fill: parent
                    island: modelData === "island" || modelData === "both"
                    bar: modelData === "taskbar" || modelData === "both"
                    off: modelData === "off"
                }
            }
        }
    }

    Label {
        visible: root.active
        Layout.topMargin: 6
        text: I18n.t("Style")
        type: "bodyLarge"
    }
    Flow {
        visible: root.active
        Layout.fillWidth: true
        spacing: 10
        Repeater {
            model: ["black", "color"]
            Choice {
                required property string modelData
                title: modelData === "black" ? I18n.t("Black") : I18n.t("Color")
                sub: modelData === "black" ? I18n.t("Like hardware, fits any theme") : I18n.t("Takes the cover’s colors")
                selected: Settings.floatingPlayerStyle === modelData
                onPicked: Settings.floatingPlayerStyle = modelData
                Rectangle {
                    anchors.fill: parent
                    color: Theme.surfaceContainerLowest
                    MiniIsland {
                        anchors.horizontalCenter: parent.horizontalCenter
                        tinted: modelData === "color"
                    }
                    MiniPill {
                        anchors.horizontalCenter: parent.horizontalCenter
                        anchors.bottom: parent.bottom
                        anchors.bottomMargin: 10
                        tinted: modelData === "color"
                    }
                }
            }
        }
    }

    // ---- Карточка выбора ----
    component Choice: Item {
        id: card
        property string title
        property string sub
        property bool selected: false
        default property alias preview: pv.data
        signal picked()
        width: 176
        height: 176

        Rectangle {
            id: face
            anchors.fill: parent
            radius: card.selected ? 28 : 16
            color: card.selected ? Theme.secondaryContainer : Theme.surfaceContainerHigh
            border.width: card.selected ? 2 : 0
            border.color: Theme.primary
            Behavior on radius { NumberAnimation { duration: Theme.motion.spatialFast; easing.type: Easing.OutBack; easing.overshoot: 1.6 } }
            Behavior on color { ColorAnimation { duration: Theme.motion.effectsDefault } }
        }
        Item {
            id: pv
            x: 10
            y: 10
            width: parent.width - 20
            height: 92
            clip: true
        }
        Column {
            x: 14
            anchors.top: pv.bottom
            anchors.topMargin: 8
            width: parent.width - 28
            spacing: 0
            Label {
                width: parent.width
                text: card.title
                type: "titleSmall"
                color: card.selected ? Theme.fgSecondaryContainer : Theme.fgSurface
                elide: Text.ElideRight
            }
            Label {
                width: parent.width
                text: card.sub
                type: "bodySmall"
                color: card.selected ? Qt.alpha(Theme.fgSecondaryContainer, 0.8) : Theme.fgSurfaceVariant
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
            }
        }
        // галочка выбранной
        Rectangle {
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.margins: 14
            width: 24
            height: 24
            radius: 12
            color: Theme.primary
            scale: card.selected ? 1 : 0
            Behavior on scale { NumberAnimation { duration: Theme.motion.spatialFast; easing.type: Easing.OutBack } }
            Icon { anchors.centerIn: parent; name: "check"; size: 16; color: Theme.fgPrimary }
        }
        StateLayer {
            anchors.fill: parent
            radius: face.radius
            cursorShape: Qt.PointingHandCursor
            onClicked: card.picked()
        }
    }

    // Рабочий стол в миниатюре: обои, панель задач, островок и/или виджет
    component MiniDesk: Item {
        id: desk
        property bool island: false
        property bool bar: false
        property bool off: false
        Rectangle {
            anchors.fill: parent
            radius: 10
            gradient: Gradient {
                GradientStop { position: 0; color: Qt.tint(Theme.surfaceContainerLow, Qt.alpha(Theme.primary, 0.22)) }
                GradientStop { position: 1; color: Qt.tint(Theme.surfaceContainerLow, Qt.alpha(Theme.tertiary, 0.18)) }
            }
        }
        // окно приложения
        Rectangle {
            x: 14; y: 22; width: parent.width * 0.5; height: parent.height * 0.5
            radius: 6
            color: Qt.alpha(Theme.surfaceContainerHighest, 0.85)
        }
        Rectangle {
            id: taskbar
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: 14
            bottomLeftRadius: 10
            bottomRightRadius: 10
            color: Qt.alpha(Theme.fgSurface, 0.1)
            Row {
                anchors.centerIn: parent
                spacing: 3
                Repeater { model: 4; Rectangle { width: 6; height: 6; radius: 2; color: Qt.alpha(Theme.fgSurface, 0.35) } }
            }
        }
        MiniIsland {
            visible: desk.island
            anchors.horizontalCenter: parent.horizontalCenter
            scale: 0.62
            transformOrigin: Item.Top
            tinted: Settings.floatingPlayerStyle === "color"
        }
        MiniPill {
            visible: desk.bar
            anchors.right: parent.right
            anchors.rightMargin: 8
            anchors.verticalCenter: taskbar.verticalCenter
            scale: 0.42
            transformOrigin: Item.Right
            tinted: Settings.floatingPlayerStyle === "color"
        }
        Icon {
            visible: desk.off
            anchors.centerIn: parent
            anchors.verticalCenterOffset: -6
            name: "visibility_off"
            size: 30
            color: Theme.fgSurfaceVariant
        }
    }

    // Компактный островок: обложка и полоски (верх прямой — растёт из кромки)
    component MiniIsland: Item {
        id: mi
        property bool tinted: false
        width: 112
        height: 26
        Rectangle {
            anchors.fill: parent
            topLeftRadius: 0
            topRightRadius: 0
            bottomLeftRadius: 13
            bottomRightRadius: 13
            color: mi.tinted ? Theme.primaryContainer : "#000000"
        }
        MiniCover { x: 7; anchors.verticalCenter: parent.verticalCenter; width: 16 }
        Row {
            anchors.right: parent.right
            anchors.rightMargin: 9
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Repeater {
                model: [0.5, 0.9, 0.65, 0.4]
                Rectangle {
                    required property real modelData
                    width: 2.5
                    height: 12 * modelData
                    radius: 1.25
                    anchors.verticalCenter: parent.verticalCenter
                    color: mi.tinted ? Theme.fgPrimaryContainer : Theme.primaryFixedDim
                }
            }
        }
    }

    // Виджет панели задач: пилюля с обложкой, названием и кнопками
    component MiniPill: Item {
        id: mp
        property bool tinted: false
        readonly property color ink: tinted ? Theme.fgPrimaryContainer : "#ffffff"
        width: 150
        height: 30
        Rectangle {
            anchors.fill: parent
            radius: 15
            color: mp.tinted ? Theme.primaryContainer : "#000000"
            border.width: 1
            border.color: Qt.rgba(1, 1, 1, 0.1)
        }
        MiniCover { x: 4; anchors.verticalCenter: parent.verticalCenter; width: 22 }
        Column {
            x: 32
            anchors.verticalCenter: parent.verticalCenter
            spacing: 3
            Rectangle { width: 50; height: 4; radius: 2; color: mp.ink }
            Rectangle { width: 34; height: 3; radius: 1.5; color: Qt.alpha(mp.ink, 0.6) }
        }
        Rectangle {
            anchors.right: parent.right
            anchors.rightMargin: 26
            anchors.verticalCenter: parent.verticalCenter
            width: 22
            height: 22
            radius: 8
            color: mp.tinted ? Theme.fgPrimaryContainer : Theme.primaryFixedDim
            Icon { anchors.centerIn: parent; name: "pause"; size: 14; fill: 1; color: mp.tinted ? Theme.primaryContainer : Theme.fgPrimaryFixed }
        }
        Icon {
            anchors.right: parent.right
            anchors.rightMargin: 6
            anchors.verticalCenter: parent.verticalCenter
            name: "skip_next"
            size: 16
            fill: 1
            color: mp.ink
        }
    }

    // Обложка играющего трека (или заглушка) в превью
    component MiniCover: Item {
        height: width
        MorphImage {
            anchors.fill: parent
            visible: Player.cover !== ""
            source: Player.cover
            shape: "cookie12"
        }
        MorphShape {
            anchors.fill: parent
            visible: Player.cover === ""
            shape: "cookie12"
            color: Theme.primary
        }
    }
}
