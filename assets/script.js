// const socket = new WebSocket('wss://www.zinfour.com/ws');

// socket.addEventListener('open', function (event) {
//     socket.send('Hello Server!');
// });

// socket.addEventListener('message', function (event) {
//     console.log('Message from server ', event.data);
// });

// setTimeout(() => {
//     const obj = { hello: "world" };
//     const blob = new Blob([JSON.stringify(obj, null, 2)], {
//       type: "application/json",
//     });
//     console.log("Sending blob over websocket");
//     socket.send(blob);
// }, 1000);

// setTimeout(() => {
//     socket.send('About done here...');
//     console.log("Sending close over websocket");
//     socket.close(3000, "Crash and Burn!");
// }, 3000);



let editing_steps = 0

let add_button = () => {
    let edit_div = document.querySelector("#editing-messages-div")
    if (editing_steps === 0) {
        let new_action = document.createElement("hr")
        edit_div.after(new_action)
        let next_action_div = document.querySelector("#next-action-div")
        next_action_div.replaceChildren()
    }

    let new_action = document.createElement("div")
    new_action.classList.add("action-msg")
    new_action.textContent = ""
    new_action.setAttribute("placeholder", "Do this...")
    new_action.contentEditable = "plaintext-only"
    edit_div.append(new_action)

    let new_story = document.createElement("div")
    new_story.classList.add("story-msg")
    new_story.textContent = ""
    new_story.setAttribute("placeholder", "You do that...")
    new_story.contentEditable = "plaintext-only"
    edit_div.append(new_story)

    editing_steps += 1
}


let edit_button = () => {

}


let discard_button = () => {
    if (editing_steps > 0) {
        let edit_div = document.querySelector("#editing-messages-div")
        let hr = document.querySelector("#messages-div > hr")
        hr.remove()
        edit_div.replaceChildren()
        editing_steps = 0
    }
}


let refresh_next_action_div = async () => {
    let next_action_div = document.querySelector("#next-action-div")
    
    let params = new URLSearchParams(document.location.search);
    let current_uuid = params.get("last_step");
    if (current_uuid == null) {
        current_uuid = window.location.pathname.substring(window.location.pathname.lastIndexOf('/') + 1)
    }

    try {
        const response = await fetch("/adventure/children/" + current_uuid);
        if (!response.ok) {
            throw new Error(`Response status: ${response.status}`);
        }

        const json = await response.json();
        let new_children = []
        i = 0
        for (const x of json) {
            let new_action = document.createElement("div")
            new_action.classList.add("action-msg")
            new_action.textContent = x[1].action
            new_action.setAttribute("onclick", "choose_action(" + i + ")")
            new_action.setAttribute("data-storytext", x[1].story)
            new_action.setAttribute("data-uuid", x[0])
            new_children.push(new_action)
            i += 1
        }
        next_action_div.replaceChildren(...new_children)

    } catch (error) {
        console.error(error.message);
    }
}

let save_button = async () => {
    let editing_msg_div = document.querySelector("#editing-messages-div")
    if (editing_msg_div.childElementCount > 0) {
        let normal_msg_div = document.querySelector("#normal-messages-div")
        while (editing_msg_div.children.length > 0) {

            editing_msg_div.firstChild.removeAttribute("contentEditable")
            let action_text = editing_msg_div.firstChild.innerText
            let action_node = editing_msg_div.firstChild
            normal_msg_div.append(action_node)

            editing_msg_div.firstChild.removeAttribute("contentEditable")
            editing_msg_div.firstChild.setAttribute("onclick", "go_back_to_story(" + (Math.ceil(normal_msg_div.childElementCount / 2 - 1)).toString() + ")")
            let story_text = editing_msg_div.firstChild.innerText
            let story_node = editing_msg_div.firstChild
            normal_msg_div.append(story_node)


            let params = new URLSearchParams(document.location.search);
            let current_uuid = params.get("last_step");
            if (current_uuid == null) {
                current_uuid = window.location.pathname.substring(window.location.pathname.lastIndexOf('/') + 1)
            }

            try {
                const response = await fetch("/adventure/new_step", {
                    method: "POST",
                    headers: {
                        "Content-Type": "application/json",
                    },
                    body: JSON.stringify({
                        action: action_text,
                        story: story_text,
                        parent: current_uuid,
                    }),
                });
                if (!response.ok) {
                    throw new Error(`Response status: ${response.status}`);
                }

                const json = await response.json();

                const url = new URL(window.location.href);
                url.searchParams.set('last_step', json.toString());
            
                window.history.replaceState(null, document.title, url)
                story_node.dataset.uuid = json
                action_node.dataset.uuid = json
            } catch (error) {
                console.error(error.message);
            }
        }
        editing_steps = 0
        let hr = document.querySelector("#messages-div > hr")
        hr.remove()

        await refresh_next_action_div()
    }
}

let go_back_to_story = async (i) => {
    let normal_msg_div = document.querySelector("#normal-messages-div")
    if (normal_msg_div.childElementCount % 2 == 0) {
        normal_msg_div.removeChild(normal_msg_div.getElementsByClassName('action-msg')[0]);
    }
    let story_action_steps = (normal_msg_div.childElementCount - 1) / 2
    if (story_action_steps - (i + 1) > 0) {
        discard_button()
        for (k = 0; k < story_action_steps - (i + 1); k++) {
            let stories = normal_msg_div.getElementsByClassName('story-msg')
            normal_msg_div.removeChild(stories[stories.length - 1]);
            let actions = normal_msg_div.getElementsByClassName('action-msg')
            normal_msg_div.removeChild(actions[actions.length - 1]);
        }
    }

    
    const url = new URL(window.location.href);
    url.searchParams.set('last_step', normal_msg_div.lastChild.dataset.uuid);

    window.history.replaceState(null, document.title, url)
    await refresh_next_action_div()
}

let go_back_to_origin = async () => {
    discard_button()
    let normal_msg_div = document.querySelector("#normal-messages-div")
    if (normal_msg_div.childElementCount > 1) {
        let next_action_div = document.querySelector("#next-action-div")
        next_action_div.replaceChildren(normal_msg_div.children[normal_msg_div.childElementCount - 2])
        normal_msg_div.replaceChildren(normal_msg_div.firstChild)
    } else {
        normal_msg_div.replaceChildren(normal_msg_div.firstChild)
    }

    window.history.replaceState(null, document.title, "/adventure/" + normal_msg_div.firstChild.dataset.uuid)
    await refresh_next_action_div()
}

let add_adventure_button = () => {
    window.location.href = "/adventure/new"
}

let choose_action = async (i) => {
    let normal_msg_div = document.querySelector("#normal-messages-div")
    let next_action_div = document.querySelector("#next-action-div")
    next_action_div.children[i].removeAttribute("onclick")
    normal_msg_div.append(next_action_div.children[i])
    next_action_div.replaceChildren()

    let new_story = document.createElement("div")
    new_story.classList.add("story-msg")
    new_story.innerText = normal_msg_div.lastChild.dataset.storytext
    new_story.setAttribute("data-uuid", normal_msg_div.lastChild.dataset.uuid)
    new_story.setAttribute("onclick", "go_back_to_story(" + (Math.ceil(normal_msg_div.childElementCount / 2 - 1)).toString() + ")")
    normal_msg_div.append(new_story)
    new_story.scrollIntoView({ behavior: "smooth" })

    const url = new URL(window.location.href);
    url.searchParams.set('last_step', normal_msg_div.lastChild.dataset.uuid);

    window.history.replaceState(null, document.title, url)
    await refresh_next_action_div()
}

let create_adventure = async () => {
    let title_text = document.querySelector("#title-header > p")
    let once_upon_a_time_div = document.querySelector(".story-msg")
    try {
        const response = await fetch("/adventure/submit_adventure", {
            method: "POST",
            headers: {
                "Content-Type": "application/json",
            },
            body: JSON.stringify({
                title: title_text.innerText,
                once_upon_a_time: once_upon_a_time_div.innerText,
            }),
        });
        if (!response.ok) {
            throw new Error(`Response status: ${response.status}`);
        }

        const json = await response.json();

        window.location.replace("/adventure/" + json.toString())

    } catch (error) {
        console.error(error.message);
    }
}

window.onload = function() {
    let normal_msg_div = document.querySelector("#normal-messages-div")
    normal_msg_div.lastChild.scrollIntoView(true);
}