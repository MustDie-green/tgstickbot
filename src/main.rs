use teloxide::{dispatching::dialogue::{InMemStorage}, prelude::*, types::{KeyboardButton, KeyboardMarkup}};
use std::vec;

type MyDialogue = Dialogue<State, InMemStorage<State>>;
type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
use teloxide::types::StickerFormat;
use teloxide::types::InputSticker;
use teloxide::types::InputFile;
use dotenv::dotenv;
use bitranslit::{transliterate, Language};
use rusqlite::{params, Connection, Result as SqlResult};


//use teloxide::types::File;

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Start,
    AwaitingAction {
        sticker_file_id: String,
    },
    GetPackName {
        sticker_file_id: String,
    },
    AddingToPack {
        sticker_file_id: String,
    },
}

#[tokio::main]
async fn main() {
    dotenv().ok(); 
    pretty_env_logger::init();
    log::info!("Starting dialogue bot...");

    let bot = Bot::from_env();

    Dispatcher::builder(
        bot,
        Update::filter_message()
            .enter_dialogue::<Message, InMemStorage<State>, State>()
            .branch(
                dptree::entry()
                    .filter(|msg: Message| msg.sticker().is_some()) // Реакция на стикеры
                    .endpoint(sticker_received)
            )
            .branch(dptree::case![State::AwaitingAction { sticker_file_id }].endpoint(receive_action))
            .branch(dptree::case![State::GetPackName { sticker_file_id }].endpoint(receive_pack_name_and_create_pack))
            .branch(dptree::case![State::AddingToPack { sticker_file_id }].endpoint(add_sticker_to_pack)),
    )
    .dependencies(dptree::deps![InMemStorage::<State>::new()])
    .enable_ctrlc_handler()
    .build()
    .dispatch()
    .await;
}


// Функция, обрабатывающая получение стикера
async fn sticker_received(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
) -> HandlerResult {
    let user_id = msg.chat.id.0;
    let chat_id = ChatId(msg.chat.id.0);
    let sticker_file_id: String = msg.sticker().unwrap().file.id.clone();

    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
    initialize_db(&conn).expect("Failed to initialize database");

    // Проверяем, есть ли уже стикерпаки у пользователя
    if let Ok(pack_list) = get_user_sticker_packs(&conn, user_id) {
        if !pack_list.is_empty() {

            let buttons = vec![
                vec![KeyboardButton::new("Добавить")],
                vec![KeyboardButton::new("Создать")],
                ];
            let keyboard = KeyboardMarkup::new(buttons).resize_keyboard();

            bot.send_message(chat_id, "Хотите добавить стикер в существующий стикерпак или создать новый?")
                .reply_markup(keyboard)
                .await?;
            
            dialogue.update(State::AwaitingAction { sticker_file_id }).await?;
        } else {
            // Если стикерпаков нет, сразу предлагаем создать новый
            bot.send_message(chat_id, "У вас нет стикерпаков. Пожалуйста, введите название нового стикерпака.").await?;
            dialogue.update(State::GetPackName { sticker_file_id }).await?;
        }
    }

    Ok(())
}

async fn receive_action(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    sticker_file_id: String,
) -> HandlerResult {
    let user_id = msg.chat.id.0;

    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
    initialize_db(&conn).expect("Failed to initialize database");
    
    match msg.text().map(ToOwned::to_owned) {
        Some(source) => {
            match source.as_str() {
                "Добавить" => {
                    if let Ok(pack_list) = get_user_sticker_packs(&conn, user_id) {
                        // Генерируем кнопки для выбора конкретного стикерпака
                        let buttons: Vec<Vec<KeyboardButton>> = pack_list
                            .iter()
                            .map(|pack_name| vec![KeyboardButton::new(pack_name.clone())])
                            .collect();
                        let keyboard = KeyboardMarkup::new(buttons).resize_keyboard();
                        
                        bot.send_message(msg.chat.id, "Окей, выбери стикерпак:")
                            .reply_markup(keyboard)
                            .await?;
    
                        dialogue.update(State::AddingToPack { sticker_file_id }).await?;
                    }
                }
                "Создать" => {
                    bot.send_message(msg.chat.id, "Окей, напиши название нового стикерпака").await?;
                    dialogue.update(State::GetPackName { sticker_file_id }).await?;
                }
                _ => {
                    bot.send_message(msg.chat.id, "Что-то пошло не так 1").await?;
                    dialogue.exit().await?
                }
            }
        }
        None => {
            bot.send_message(msg.chat.id, "Что-то пошло не так 2")
                .await?;
            dialogue.exit().await?
        }
    }
    Ok(())
}

// Функция для создания нового стикерпака с помощью Telegram Bot API
async fn receive_pack_name_and_create_pack(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
) -> HandlerResult {
    let state = dialogue.get().await?;

    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
    initialize_db(&conn).expect("Failed to initialize database");

    if let Some(State::GetPackName { sticker_file_id }) = state {
        let user_id: i64 = msg.chat.id.0;
        let user_id_2 = UserId(user_id as u64);
        let chat_id = ChatId(msg.chat.id.0);
        let sticker_file_id: String = sticker_file_id.clone();

        // Обработка имени стикерпакета
        if let Some(pack_name) = msg.text() {
            let id_pack_name = process_string(&format!("{pack_name}_by_flex_stickerpack_bot"));

            // Сохраняем информацию о стикерпаке в базу данных
            save_sticker_pack(&conn, user_id, &pack_name, &id_pack_name).expect("Failed to save sticker pack");

            let sticker = vec![InputSticker {
                sticker: InputFile::file_id(sticker_file_id),
                emoji_list: vec!["💬".to_string()],
                mask_position: None,
                keywords: vec!["quote".to_string()],
            }];

            bot.send_message(chat_id, format!("Создаем новый стикерпак: {id_pack_name}, название: {pack_name}")).await?;
    
            // Создание нового стикерпакета с использованием Telegram API
            let result = bot
                .create_new_sticker_set(
                    user_id_2,
                    id_pack_name.clone(), // Уникальное имя стикерпакета
                    pack_name, // Название
                    sticker, // Стикеры
                    StickerFormat::Static, // Формат стикера
                )
                .await;
    
            match result {
                Ok(_) => {
                    bot.send_message(chat_id, format!("Держи свой стикерпак t.me/addstickers/{id_pack_name}")).await?;
                }
                Err(err) => {
                    bot.send_message(chat_id, format!("Не удалось создать стикерпак: {err}")).await?;
                }
            }
    
            dialogue.exit().await?;
        }
    } else {
        return Ok(());
    }

    Ok(())
}

// Функция для добавления стикера в существующий стикерпак
async fn add_sticker_to_pack(
    bot: Bot,
    dialogue: MyDialogue,
    sticker_file_id :String,
    msg: Message,
) -> HandlerResult {
    let user_id = msg.chat.id.0;
    let user_id_2 = UserId(user_id as u64);
    let sticker = InputSticker {
        sticker: InputFile::file_id(sticker_file_id),
        emoji_list: vec!["💬".to_string()],
        mask_position: None,
        keywords: vec!["quote".to_string()],
    };

    match msg.text().map(ToOwned::to_owned) {
        Some(source) => {
            let pack_name = source.as_str();
            let result = bot.add_sticker_to_set(
                user_id_2,
                pack_name, // Уникальное имя стикерпакета
                sticker,  // ID стикера
            )
            .await;

            match result {
                Ok(_) => {
                    bot.send_message(msg.chat.id, format!("Стикер добавлен в стикерпак t.me/addstickers/{pack_name}.")).await?;
                }
                Err(err) => {
                    bot.send_message(msg.chat.id, format!("Не удалось добавить стикер в стикерпак: {err}")).await?;
                }
            }
            dialogue.exit().await?;

        }
        None => {
            bot.send_message(msg.chat.id, "Что-то пошло не так 3")
                .await?;
            dialogue.exit().await?
        }
    }
    Ok(())

}

fn process_string(input: &str) -> String {
    // Заменяем пробелы на подчёркивания
    let replaced = input.replace(" ", "_");
    
    // Транслитерируем строку
    let output = transliterate(&replaced, Language::Russian, false);

    output
}

// Инициализация базы данных SQLite
fn initialize_db(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sticker_packs (
            user_id INTEGER,
            pack_name TEXT,
            id_pack_name TEXT,
            PRIMARY KEY (user_id, id_pack_name)
        )",
        [],
    )?;
    Ok(())
}

// Функция, сохраняющая стикерпак в базу данных
fn save_sticker_pack(conn: &Connection, user_id: i64, pack_name: &str, id_pack_name: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO sticker_packs (user_id, pack_name, id_pack_name) VALUES (?1, ?2, ?3)",
        params![user_id, pack_name, id_pack_name],
    )?;
    Ok(())
}

// Функция, получающая список стикерпаков пользователя
fn get_user_sticker_packs(conn: &Connection, user_id: i64) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id_pack_name FROM sticker_packs WHERE user_id = ?1")?;
    let packs_iter = stmt.query_map([user_id], |row| row.get(0))?;

    let mut packs = Vec::new();
    for pack in packs_iter {
        packs.push(pack?);
    }
    Ok(packs)
}