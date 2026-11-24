import QtQuick
import QtQuick.Layouts
import Md3
import Materialfy.Core

// Правая выдвижная панель: очередь / текст песни / устройства Connect.
Surface {
    id: root

    // Вкладки (Settings.rightPanelTab). Подпись — только у выбранной: три подписи в 340 px не помещаются.
    readonly property var tabs: [
        { key: "queue", text: I18n.t("Queue"), icon: "queue_music" },
        { key: "lyrics", text: I18n.t("Song lyrics"), icon: "title" },
        { key: "devices", text: I18n.t("Devices"), icon: "speaker_group" },
        { key: "friends", text: I18n.t("Friends"), icon: "group" }
    ]
    readonly property int tabIndex: Math.max(0, tabs.findIndex(t => t.key === Settings.rightPanelTab))
    readonly property string tab: tabs[tabIndex].key

    level: "container"
    translucency: Settings.ambientBackground && Player.cover !== "" ? 0.3 : 0

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 16
        anchors.bottomMargin: 0
        spacing: 12

        RowLayout {
            Layout.fillWidth: true
            ButtonGroup {
                model: root.tabs.map((t, i) => ({ icon: t.icon, text: i === root.tabIndex ? t.text : "" }))
                autoSelect: false
                currentIndex: root.tabIndex
                onActivated: i => Settings.rightPanelTab = root.tabs[i].key
            }
            Item { Layout.fillWidth: true }
            IconButton { icon: "close"; onClicked: Settings.rightPanelOpen = false }
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            QueueView {
                anchors.fill: parent
                visible: root.tab === "queue"
            }
            LyricsView {
                anchors.fill: parent
                anchors.bottomMargin: 16
                visible: root.tab === "lyrics" && Settings.rightPanelOpen
                baseFontSize: 22
                anchorPos: 0.3
            }
            // загружается при первом открытии вкладки
            Loader {
                anchors.fill: parent
                active: root.tab === "devices" || item !== null
                visible: root.tab === "devices"
                sourceComponent: DevicePanel {
                    shown: root.tab === "devices" && Settings.rightPanelOpen
                }
            }
            // друзья и Jam: тоже при первом открытии; опрос активности — только пока вкладка видна
            Loader {
                anchors.fill: parent
                active: root.tab === "friends" || item !== null
                visible: root.tab === "friends"
                sourceComponent: FriendsPanel {
                    shown: root.tab === "friends" && Settings.rightPanelOpen
                }
            }
        }
    }
}
