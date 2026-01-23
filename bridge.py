import time
import threading
from pywinauto import Desktop
from pythonosc import dispatcher, osc_server, udp_client

# === КОНФИГУРАЦИЯ ===
RUST_PORT = 4455       # Порт, куда Rust шлет инфу о деке
NODE_PORT = 4460       # Порт RekordKaraoke (Node.js)
POLL_INTERVAL = 0.5    # Как часто проверять экран (сек)

# Глобальная переменная: Какая дека сейчас главная? (0 или 1)
current_master_deck = 0 
last_sent_track_info = ""

# Клиент для отправки в Node.js
sender = udp_client.SimpleUDPClient("127.0.0.1", NODE_PORT)

# === ЧТЕНИЕ ЭКРАНА ===
class UIReader:
    def __init__(self):
        self.app_title = ".*rekordbox.*"
        self.anchor = "2Deck Horizontal"
        # Индексы элементов (Title, Artist)
        self.d1_indices = (133, 135) 
        self.d2_indices = (156, 158)
        self.window = None
        self.container = None

    def connect(self):
        try:
            desktop = Desktop(backend="uia")
            windows = desktop.windows(title_re=self.app_title)
            self.window = next((w for w in windows if w.is_visible()), None)
            
            if self.window:
                # Ищем контейнер с текстом "2Deck Horizontal"
                for el in self.window.descendants():
                    if el.window_text() == self.anchor:
                        self.container = el.parent()
                        print("[UI] Connected to Rekordbox Window")
                        return True
            return False
        except Exception:
            return False

    def get_track_info(self, deck_index):
        # Если связь с окном потеряна - пробуем переподключиться
        if not self.container:
            if not self.connect(): return None
        
        try:
            children = self.container.children()
            # Проверка, не сбилась ли верстка
            if len(children) < 160: 
                self.connect()
                return None
            
            # Выбираем индексы в зависимости от деки
            indices = self.d1_indices if deck_index == 0 else self.d2_indices
            
            track = children[indices[0]].window_text()
            artist = children[indices[1]].window_text()
            
            return artist, track
        except Exception:
            self.container = None # Сброс кеша
            return None

reader = UIReader()

# === ПОТОК 1: СЛУШАЕМ RUST ===
def handle_deck_change(address, *args):
    global current_master_deck
    try:
        new_deck = int(args[0])
        if new_deck in [0, 1]:
            # Просто обновляем переменную, не отправляем ничего
            if current_master_deck != new_deck:
                print(f"[OSC] Rust switched master to Deck {new_deck + 1}")
                current_master_deck = new_deck
    except ValueError:
        pass

def start_osc_listener():
    dp = dispatcher.Dispatcher()
    dp.map("/deck/master", handle_deck_change)
    
    # Слушаем 0.0.0.0 для надежности
    server = osc_server.ThreadingOSCUDPServer(("0.0.0.0", RUST_PORT), dp)
    print(f"[Proxy] Listening for Deck Info on {RUST_PORT}...")
    server.serve_forever()

# === ПОТОК 2: ПРОВЕРЯЕМ ЭКРАН (POLLING) ===
def start_polling():
    global last_sent_track_info
    print(f"[Poller] Started checking UI every {POLL_INTERVAL}s...")
    
    while True:
        try:
            # Читаем текст из ТЕКУЩЕЙ мастер-деки
            res = reader.get_track_info(current_master_deck)
            
            if res:
                artist, title = res
                combined = f"{artist} - {title}"
                
                # Если текст изменился по сравнению с тем, что мы отправляли последний раз
                if combined != last_sent_track_info and artist and title:
                    print(f">>> DETECTED NEW TRACK: {combined}")
                    
                    # Отправляем в караоке
                    sender.send_message("/track/master/artist", artist)
                    sender.send_message("/track/master/title", title)
                    
                    last_sent_track_info = combined
            
        except Exception as e:
            print(f"[Poller Error] {e}")
            
        time.sleep(POLL_INTERVAL)

# === MAIN ===
if __name__ == "__main__":
    # Запускаем слушателя Rust в фоне
    t = threading.Thread(target=start_osc_listener, daemon=True)
    t.start()
    
    # Запускаем опрос экрана в главном потоке
    start_polling()